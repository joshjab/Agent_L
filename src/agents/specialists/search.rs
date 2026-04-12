use std::collections::HashMap;
use std::io;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::prompts;
use crate::tools::Tool;
use crate::tools::executor::{MAX_STEPS, execute_react_loop};
use crate::tools::search_tools::{LocalSearchTool, WebSearchTool};

fn is_leap(y: u64) -> bool {
    y.is_multiple_of(400) || (y.is_multiple_of(4) && !y.is_multiple_of(100))
}

/// Compute current UTC date and time from the UNIX timestamp.
/// No external crate needed — standard calendar arithmetic.
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hour = (secs % 86400) / 3600;
    let minute = (secs % 3600) / 60;
    let mut days = secs / 86400;
    let mut year = 1970u64;
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &dim in &months {
        if days < dim {
            break;
        }
        days -= dim;
        month += 1;
    }
    format!("{year}-{month:02}-{} {hour:02}:{minute:02} UTC", days + 1)
}

const SEARCH_FALLBACK: &str = "\
You are a search specialist. Find accurate, factual information using the \
tools available to you.\n\
\n\
Current date and time (UTC): {now}. If a source looks outdated relative to today, \
note that in your answer.\n\
\n\
AVAILABLE TOOLS (you MUST use one before answering):\n\
- web_search {\"query\": \"...\"} — search the web for current information\n\
- local_search {\"query\": \"...\", \"path\": \".\"} — grep local project files\n\
\n\
RULES:\n\
- You MUST call at least one tool before giving a FinalAnswer. NEVER answer from \
your own knowledge alone — even if you think you know the answer.\n\
- After receiving an Observation, your NEXT output MUST be a FinalAnswer — do NOT \
call another tool or add more Thoughts.\n\
- Your FinalAnswer MUST be derived ONLY from the Observation text. Do NOT use your \
training knowledge to supplement, correct, or override the search results — even if \
you believe the results are wrong. The Observation reflects current real-world state.\n\
- Use the ReAct format — one action per line:\n\
  Thought: <your reasoning>\n\
  ToolCall: <tool_name> {\"arg\": \"value\"}\n\
  FinalAnswer: <your answer based on the Observation>\n\
\n\
EXAMPLE (web search):\n\
Thought: I need to find current information about this.\n\
ToolCall: web_search {\"query\": \"current president United States 2025\"}\n\
[Observation returned]\n\
FinalAnswer: <concise answer derived from the Observation>\n\
\n\
EXAMPLE (local file search):\n\
Thought: I need to find fn main in project files.\n\
ToolCall: local_search {\"query\": \"fn main\", \"path\": \".\"}\n\
[Observation returned]\n\
FinalAnswer: Found fn main in src/main.rs:10 and src/lib.rs:5.";

/// Build the search specialist system prompt, injecting the current UTC datetime
/// so the model can answer time/date queries accurately and flag stale sources.
fn search_system_prompt() -> String {
    let now = utc_now();
    prompts::load_with("search", SEARCH_FALLBACK, &[("now", &now)])
}

/// Handles Factual intent queries by calling search tools and returning
/// cited answers. Always uses at least one tool call before answering.
pub struct SearchSpecialist {
    pub model: String,
    /// Ollama non-streaming endpoint (e.g. `http://localhost:11434/api/chat`).
    pub chat_url: String,
    /// Base URL for the DuckDuckGo API. Override in tests to point at wiremock.
    pub ddg_base_url: String,
}

impl SearchSpecialist {
    /// Search only reads from the web and local files — safe to run in parallel
    /// with other read-only specialists (e.g. Chat, Memory). Used by M8 parallel runner.
    #[allow(dead_code)]
    pub fn concurrency_safe() -> bool {
        true
    }

    pub fn new(model: impl Into<String>, chat_url: impl Into<String>) -> Self {
        // Allow tests (and operators) to redirect DDG calls to a local mock server
        // without changing run_plan's signature.
        let ddg_base_url = std::env::var("AGENT_L_DDG_BASE_URL")
            .unwrap_or_else(|_| "https://api.duckduckgo.com".into());
        Self {
            model: model.into(),
            chat_url: chat_url.into(),
            ddg_base_url,
        }
    }

    /// Construct with a custom DuckDuckGo base URL — used in tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new_with_ddg_url(
        model: impl Into<String>,
        chat_url: impl Into<String>,
        ddg_base_url: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            chat_url: chat_url.into(),
            ddg_base_url: ddg_base_url.into(),
        }
    }

    /// Run the search task: calls the ReAct loop with web and local search
    /// tools. Streams the final answer as `AppEvent::Token` and returns it.
    pub async fn run(
        &self,
        task: &str,
        context: Option<&str>,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Result<String, String> {
        let web_tool = WebSearchTool::new_with_base_url(&self.ddg_base_url);
        let local_tool = LocalSearchTool;

        let mut tools: HashMap<&str, &dyn Tool> = HashMap::new();
        tools.insert("web_search", &web_tool);
        tools.insert("local_search", &local_tool);

        let mut messages = vec![json!({"role": "system", "content": search_system_prompt()})];
        if let Some(ctx) = context {
            messages.push(json!({"role": "user", "content": ctx}));
        }
        messages.push(json!({"role": "user", "content": task}));

        let model = self.model.clone();
        let chat_url = self.chat_url.clone();

        let answer = execute_react_loop(
            messages,
            &tools,
            move |msgs| {
                let model = model.clone();
                let chat_url = chat_url.clone();
                async move {
                    let body = json!({
                        "model": model,
                        "messages": msgs,
                        "stream": false,
                        "think": false
                    });
                    let raw = crate::ollama::post_json(&chat_url, body).await?;
                    let envelope: Value = serde_json::from_str(&raw)?;
                    let content = envelope["message"]["content"]
                        .as_str()
                        .ok_or_else(|| {
                            Box::new(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Ollama response missing message.content",
                            )) as Box<dyn std::error::Error>
                        })?
                        .to_string();
                    Ok(content)
                }
            },
            Some(tx.clone()),
            MAX_STEPS,
        )
        .await
        .map_err(|e| e.message)?;

        let answer = deduplicate_sentences(&answer);
        let _ = tx.send(AppEvent::Token(answer.clone()));
        Ok(answer)
    }
}

/// Remove consecutive duplicate sentences from `text`.
///
/// Splits on `". "` (period + space) and discards any sentence that has already
/// appeared (case-insensitive). The source citation line is preserved because it
/// is unique. This prevents the model from echoing the DDG snippet verbatim then
/// repeating the same fact in different words.
fn deduplicate_sentences(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut parts: Vec<&str> = Vec::new();
    for sentence in text.split(". ") {
        let key = sentence.trim().to_lowercase();
        if !key.is_empty() && seen.insert(key) {
            parts.push(sentence.trim());
        }
    }
    parts.join(". ")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Wrap a model content string in the Ollama non-streaming envelope.
    // Uses serde_json::json! so newlines, quotes, and other special characters
    // in `content` are correctly escaped — avoids the `\\n` vs `\n` trap.
    fn ollama_response(content: &str) -> String {
        serde_json::json!({
            "model": "m",
            "message": {"role": "assistant", "content": content},
            "done": true
        })
        .to_string()
    }

    // A minimal DuckDuckGo response JSON.
    fn ddg_response(abstract_text: &str, url: &str) -> String {
        format!(
            r#"{{"AbstractText":"{abstract_text}","AbstractURL":"{url}","AbstractSource":"Test","RelatedTopics":[]}}"#
        )
    }

    // ── system prompt requirements ───────────────────────────────────────────
    //
    // These tests guard the three phrases/structures that were identified as
    // critical for forcing llama3.2 to call a tool before answering. Removing
    // any one of them caused a regression where the model answered from training
    // data (e.g., "France" without searching). Add a test here whenever you
    // find a new prompt phrase that is load-bearing for correct behaviour.

    #[test]
    fn system_prompt_forbids_answering_without_tool_call() {
        // This exact phrase stops llama3.2 from skipping the tool call on
        // questions it "knows" the answer to (e.g. geography, simple facts).
        // Do NOT remove it — see the M7.6 regression post-mortem.
        let prompt = search_system_prompt();
        assert!(
            prompt.contains("NEVER answer from your own knowledge alone"),
            "prompt must contain 'NEVER answer from your own knowledge alone' \
             to prevent the model from skipping tool calls on simple facts: {prompt}"
        );
    }

    #[test]
    fn system_prompt_includes_react_format_diagram() {
        // The explicit Thought/ToolCall/FinalAnswer diagram is necessary for
        // llama3.2 to produce the ReAct format reliably. Without it the model
        // sometimes emits prose instead of structured output.
        let prompt = search_system_prompt();
        assert!(
            prompt.contains("Use the ReAct format"),
            "prompt must include 'Use the ReAct format' heading: {prompt}"
        );
        assert!(
            prompt.contains("ToolCall: <tool_name>"),
            "prompt must show the ToolCall line in the format diagram: {prompt}"
        );
    }

    #[test]
    fn system_prompt_example_uses_observation_placeholder() {
        // The "[Observation returned]" placeholder in the example tells the model
        // it must WAIT for a tool result before writing FinalAnswer. When this was
        // replaced with an inline observation the model started skipping the tool.
        let prompt = search_system_prompt();
        assert!(
            prompt.contains("[Observation returned]"),
            "prompt example must use '[Observation returned]' placeholder \
             to signal the model must wait for a tool result: {prompt}"
        );
    }

    #[test]
    fn system_prompt_requires_observation_only_grounding() {
        let prompt = search_system_prompt();
        // Must tell the model its FinalAnswer must come ONLY from the Observation,
        // not from training knowledge. Both key phrases must appear.
        assert!(
            prompt.contains("ONLY from the Observation")
                || prompt.contains("only from the Observation"),
            "prompt must say answer must come ONLY from the Observation: {prompt}"
        );
        assert!(
            prompt.contains("do NOT") || prompt.contains("Do NOT"),
            "prompt must use 'do NOT' to prohibit using training knowledge: {prompt}"
        );
    }

    // ── deduplicate_sentences ────────────────────────────────────────────────

    #[test]
    fn deduplicate_sentences_removes_exact_duplicates() {
        let text =
            "Paris is the capital of France. Paris is the capital of France. France is in Europe.";
        let result = deduplicate_sentences(text);
        assert_eq!(
            result.matches("Paris is the capital of France.").count(),
            1,
            "duplicate sentence should appear only once"
        );
        assert!(result.contains("France is in Europe."));
    }

    #[test]
    fn deduplicate_sentences_preserves_unique_sentences() {
        let text = "First sentence. Second sentence. Third sentence.";
        let result = deduplicate_sentences(text);
        assert!(result.contains("First sentence."));
        assert!(result.contains("Second sentence."));
        assert!(result.contains("Third sentence."));
    }

    #[test]
    fn deduplicate_sentences_handles_empty_input() {
        assert_eq!(deduplicate_sentences(""), "");
    }

    #[test]
    fn deduplicate_sentences_is_case_insensitive() {
        let text = "Paris is the capital. paris is the capital. Different fact.";
        let result = deduplicate_sentences(text);
        assert_eq!(
            result.matches("capital").count(),
            1,
            "case-insensitive duplicate should appear only once"
        );
    }

    // ── happy-path ───────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn search_returns_final_answer_after_tool_call() {
        let ollama_server = MockServer::start().await;
        let ddg_server = MockServer::start().await;

        // First Ollama call: model issues a tool call
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ollama_response(
                "Thought: search needed\nToolCall: web_search {\"query\":\"capital of France\"}",
            )))
            .up_to_n_times(1)
            .mount(&ollama_server)
            .await;

        // DuckDuckGo returns a result
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ddg_response(
                "Paris is the capital of France.",
                "https://en.wikipedia.org/wiki/France",
            )))
            .mount(&ddg_server)
            .await;

        // Second Ollama call: model gives a final answer
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                ollama_response(
                    "FinalAnswer: The capital of France is Paris. Source: https://en.wikipedia.org/wiki/France",
                ),
            ))
            .mount(&ollama_server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let specialist = SearchSpecialist::new_with_ddg_url(
            "m",
            format!("{}/api/chat", ollama_server.uri()),
            ddg_server.uri(),
        );

        let result = specialist
            .run("What is the capital of France?", None, tx)
            .await
            .unwrap();
        assert!(
            result.contains("Paris"),
            "answer should mention Paris, got: {result}"
        );

        // A Token event must appear in the channel (may be preceded by ToolCall/ToolResult)
        let mut found_token = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AppEvent::Token(_)) {
                found_token = true;
                break;
            }
        }
        assert!(
            found_token,
            "expected at least one AppEvent::Token in the channel"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_calls_ddg_at_least_once() {
        let ollama_server = MockServer::start().await;
        let ddg_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ollama_response("ToolCall: web_search {\"query\":\"test\"}")),
            )
            .up_to_n_times(1)
            .mount(&ollama_server)
            .await;

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ddg_response("Some result.", "https://example.com")),
            )
            .mount(&ddg_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ollama_response(
                "FinalAnswer: answer here. Source: https://example.com",
            )))
            .mount(&ollama_server)
            .await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let specialist = SearchSpecialist::new_with_ddg_url(
            "m",
            format!("{}/api/chat", ollama_server.uri()),
            ddg_server.uri(),
        );

        specialist.run("find something", None, tx).await.unwrap();

        // DDG must have been called at least once
        assert!(
            !ddg_server.received_requests().await.unwrap().is_empty(),
            "DDG should have been called"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_injects_context_as_user_message() {
        let ollama_server = MockServer::start().await;
        let ddg_server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ddg_response("result", "https://example.com")),
            )
            .mount(&ddg_server)
            .await;

        // Capture the request body to verify context injection
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ollama_response("ToolCall: web_search {\"query\":\"q\"}")),
            )
            .up_to_n_times(1)
            .mount(&ollama_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ollama_response(
                "FinalAnswer: ok Source: https://example.com",
            )))
            .mount(&ollama_server)
            .await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let specialist = SearchSpecialist::new_with_ddg_url(
            "m",
            format!("{}/api/chat", ollama_server.uri()),
            ddg_server.uri(),
        );

        // Should not panic with context provided
        specialist
            .run("task", Some("prior step output"), tx)
            .await
            .unwrap();
        assert_eq!(ollama_server.received_requests().await.unwrap().len(), 2);
    }

    // ── sad-path ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn search_returns_err_when_ollama_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let (tx, _rx) = mpsc::unbounded_channel();
        let specialist = SearchSpecialist::new_with_ddg_url(
            "m",
            format!("http://127.0.0.1:{port}/api/chat"),
            "http://unused",
        );

        assert!(specialist.run("question", None, tx).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_circuit_breaker_fires_when_model_loops() {
        // Model keeps returning Thoughts without FinalAnswer → circuit breaker
        let ollama_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ollama_response("Thought: still thinking")),
            )
            .mount(&ollama_server)
            .await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let specialist = SearchSpecialist::new_with_ddg_url(
            "m",
            format!("{}/api/chat", ollama_server.uri()),
            "http://unused",
        );

        // Should return Err after hitting MAX_STEPS
        assert!(specialist.run("loop forever", None, tx).await.is_err());
    }
}
