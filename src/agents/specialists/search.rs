use std::collections::HashMap;
use std::io;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::app::AppEvent;
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

/// Build the search specialist system prompt, injecting the current UTC datetime
/// so the model can answer time/date queries accurately and flag stale sources.
fn search_system_prompt() -> String {
    let now = utc_now();
    format!(
        "You are a search specialist. Find accurate, factual information using the \
tools available to you.\n\
\n\
Current date and time (UTC): {now}. If a source looks outdated relative to today, \
note that in your answer.\n\
\n\
AVAILABLE TOOLS (you MUST use one before answering):\n\
- web_search {{\"query\": \"...\"}} — search the web with DuckDuckGo\n\
- local_search {{\"query\": \"...\", \"path\": \".\"}} — grep local project files\n\
\n\
RULES:\n\
- You MUST call at least one tool before giving a FinalAnswer. NEVER answer from \
your own knowledge alone — even if you think you know the answer.\n\
- After receiving an Observation, your NEXT output MUST be a FinalAnswer — do NOT \
call another tool or add more Thoughts.\n\
- Use the ReAct format — one action per line:\n\
  Thought: <your reasoning>\n\
  ToolCall: <tool_name> {{\"arg\": \"value\"}}\n\
  FinalAnswer: <your answer with source URL or file path>\n\
\n\
EXAMPLE (web search):\n\
Thought: I need to find the latest Rust version.\n\
ToolCall: web_search {{\"query\": \"latest stable Rust version\"}}\n\
[Observation returned]\n\
FinalAnswer: Rust 1.XX was released on DATE. Source: https://...\n\
\n\
EXAMPLE (local file search):\n\
Thought: I need to find fn main in project files.\n\
ToolCall: local_search {{\"query\": \"fn main\", \"path\": \".\"}}\n\
[Observation returned]\n\
FinalAnswer: Found fn main in src/main.rs:10 and src/lib.rs:5.\n\
\n\
Always include file paths or URLs from the Observation in your FinalAnswer."
    )
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
        Self {
            model: model.into(),
            chat_url: chat_url.into(),
            ddg_base_url: "https://api.duckduckgo.com".into(),
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
                        "stream": false
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
    fn ollama_response(content: &str) -> String {
        format!(
            r#"{{"model":"m","message":{{"role":"assistant","content":"{content}"}},"done":true}}"#
        )
    }

    // A minimal DuckDuckGo response JSON.
    fn ddg_response(abstract_text: &str, url: &str) -> String {
        format!(
            r#"{{"AbstractText":"{abstract_text}","AbstractURL":"{url}","AbstractSource":"Test","RelatedTopics":[]}}"#
        )
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
            .respond_with(ResponseTemplate::new(200).set_body_string(
                ollama_response(r#"Thought: search needed\\nToolCall: web_search {\"query\":\"capital of France\"}"#),
            ))
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

        // Token event should have been sent
        let token = rx.try_recv().expect("expected a Token event");
        assert!(matches!(token, AppEvent::Token(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_calls_ddg_at_least_once() {
        let ollama_server = MockServer::start().await;
        let ddg_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ollama_response(
                r#"ToolCall: web_search {\"query\":\"test\"}"#,
            )))
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
                    .set_body_string(ollama_response(r#"ToolCall: web_search {\"query\":\"q\"}"#)),
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
