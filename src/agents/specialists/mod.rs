pub mod chat;
pub mod code;
pub mod search;

use std::path::Path;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::agents::orchestrator::{AgentKind, TaskPlan};
use crate::app::AppEvent;

use chat::ChatSpecialist;

// ─── Synthesis constants ─────────────────────────────────────────────────────

const SYNTHESIS_FALLBACK: &str = "You are Agent-L, a local personal assistant. \
You have been given verified output from a specialist (tagged by source type). \
Present this information to the user in Agent-L's voice — concise, direct, and \
natural. Do not add facts, editorialize, or contradict the specialist output. \
Do not mention that you are synthesizing or that a specialist was called.";

fn synthesis_system_prompt() -> String {
    crate::prompts::load("persona_synthesis", SYNTHESIS_FALLBACK)
}

/// Map an `AgentKind` to its tagged-context label so the synthesis model knows
/// what type of content it is presenting.
fn agent_source_tag(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::Search => "[SEARCH RESULT]",
        AgentKind::Code => "[CODE OUTPUT]",
        AgentKind::Memory => "[MEMORY RESULT]",
        AgentKind::Shell => "[SHELL OUTPUT]",
        _ => "[SPECIALIST OUTPUT]",
    }
}

/// Build the tagged context string that is passed to the synthesis specialist.
///
/// Each specialist output is prefixed with a source tag so the persona model
/// understands what kind of content it is presenting. Multiple outputs are
/// separated by a blank line.
pub(crate) fn build_synthesis_context(outputs: &[(AgentKind, String)]) -> String {
    if outputs.is_empty() {
        return String::new();
    }
    outputs
        .iter()
        .map(|(kind, text)| format!("{}\n{}", agent_source_tag(kind), text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Returns a sender whose Token events are silently dropped before reaching
/// `real_tx`. All other events (`ToolCall`, `ToolResult`, etc.) pass through.
///
/// Used when a synthesis step will re-stream specialist output in Agent-L's
/// voice — we don't want the raw specialist tokens reaching the UI first.
/// The background forwarding task exits automatically when the returned sender
/// is dropped (i.e. when the specialist finishes).
fn make_filtering_tx(real_tx: mpsc::UnboundedSender<AppEvent>) -> mpsc::UnboundedSender<AppEvent> {
    let (filter_tx, mut filter_rx) = mpsc::unbounded_channel::<AppEvent>();
    tokio::spawn(async move {
        while let Some(event) = filter_rx.recv().await {
            match event {
                AppEvent::Token(_) => {} // suppressed; synthesis re-streams
                other => {
                    let _ = real_tx.send(other);
                }
            }
        }
    });
    filter_tx
}

/// Returned when a specialist call fails.
#[derive(Debug)]
pub struct SpecialistError {
    pub message: String,
}

impl std::fmt::Display for SpecialistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "specialist error: {}", self.message)
    }
}

impl std::error::Error for SpecialistError {}

/// Execute all steps in `plan` in order, injecting `depends_on` context where
/// needed. Sends `StreamDone` to `tx` after all steps complete (or on the
/// first error). Returns `Err` only on unrecoverable failures.
///
/// After all non-Chat specialist steps complete, a synthesis step runs a
/// lightweight Chat call to present the specialist output in Agent-L's voice.
/// Outputs shorter than `SYNTHESIS_MIN_CHARS` are streamed directly (no
/// round-trip). Unknown specialist kinds fall back to `ChatSpecialist`.
pub async fn run_plan(
    plan: &TaskPlan,
    messages: &[Value],
    model: &str,
    chat_url: &str,
    working_dir: &Path,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<(), SpecialistError> {
    // Collect per-step outputs for `depends_on` injection.
    let mut step_outputs: Vec<Option<String>> = Vec::with_capacity(plan.steps.len());
    let mut had_streaming_step = false;

    // Outputs collected from non-Chat specialists for the synthesis step.
    let mut specialist_outputs: Vec<(AgentKind, String)> = Vec::new();

    // Pre-scan: does this plan have any step that warrants synthesis?
    // Shell/Calendar/Memory placeholders are short enough that the threshold
    // rejects them anyway, but we restrict here for clarity.
    let has_synthesis_candidate = plan
        .steps
        .iter()
        .any(|s| matches!(s.agent, AgentKind::Search | AgentKind::Code));

    for step in &plan.steps {
        // Resolve context from a prior step if requested.
        let context: Option<&str> = step
            .depends_on
            .and_then(|idx| step_outputs.get(idx))
            .and_then(|opt| opt.as_deref());

        // For specialist steps that will be synthesized, use a filtering tx so
        // raw Token events don't reach the UI before synthesis re-streams them.
        // ToolCall / ToolResult events still pass through to show search activity.
        let step_tx = if has_synthesis_candidate
            && matches!(step.agent, AgentKind::Search | AgentKind::Code)
        {
            make_filtering_tx(tx.clone())
        } else {
            tx.clone()
        };

        let output = match step.agent {
            AgentKind::Chat | AgentKind::Unknown => {
                had_streaming_step = true;
                ChatSpecialist
                    .run(&step.task, messages, context, model, chat_url, step_tx)
                    .await
                    .inspect_err(|_| {
                        let _ = tx.send(AppEvent::StreamDone);
                    })?
            }
            AgentKind::Code => {
                had_streaming_step = true;
                let specialist = code::CodeSpecialist::new(model, chat_url, working_dir);
                let output = specialist.run(&step.task, step_tx).await.map_err(|msg| {
                    let _ = tx.send(AppEvent::StreamDone);
                    SpecialistError { message: msg }
                })?;
                // Collect for synthesis; don't stream the raw output directly.
                if !output.is_empty() {
                    specialist_outputs.push((AgentKind::Code, output.clone()));
                }
                output
            }
            AgentKind::Search => {
                had_streaming_step = true;
                let specialist = search::SearchSpecialist::new(model, chat_url);
                let out = specialist
                    .run(&step.task, context, step_tx)
                    .await
                    .map_err(|msg| {
                        let _ = tx.send(AppEvent::StreamDone);
                        SpecialistError { message: msg }
                    })?;
                // Collect for synthesis; don't stream the raw output directly.
                if !out.is_empty() {
                    specialist_outputs.push((AgentKind::Search, out.clone()));
                }
                out
            }
            // Not yet implemented — return a silent placeholder so `depends_on`
            // chains still get *something*, but don't stream duplicate responses.
            AgentKind::Shell | AgentKind::Calendar | AgentKind::Memory => {
                format!("[{:?} specialist not yet implemented]", step.agent)
            }
        };

        step_outputs.push(Some(output));
    }

    // ── Synthesis step ───────────────────────────────────────────────────────
    // Present all collected specialist outputs in Agent-L's voice.
    // Always synthesize — even short answers benefit from consistent voice,
    // and it prevents source tags from leaking to the user.
    if !specialist_outputs.is_empty() {
        let combined = build_synthesis_context(&specialist_outputs);
        if !combined.is_empty() {
            // Build a self-contained message list: synthesis system prompt +
            // tagged specialist output as the user turn. The persona.md system
            // prompt is intentionally NOT used here — it would tell the model it
            // doesn't have current facts, which is wrong when search results are
            // right in front of it.
            let synthesis_msgs = vec![
                json!({"role": "system", "content": synthesis_system_prompt()}),
                json!({"role": "user", "content": combined}),
            ];
            ChatSpecialist
                .run("", &synthesis_msgs, None, model, chat_url, tx.clone())
                .await
                .inspect_err(|_| {
                    let _ = tx.send(AppEvent::StreamDone);
                })?;
        }
        had_streaming_step = true;
    }

    // If no step streamed a response (e.g. a plan with only unimplemented
    // specialists), fall back to a single Chat call so the user always gets
    // an answer.
    if !had_streaming_step && !plan.steps.is_empty() {
        let fallback_task = plan.steps[0].task.as_str();
        ChatSpecialist
            .run(fallback_task, messages, None, model, chat_url, tx.clone())
            .await
            .inspect_err(|_| {
                let _ = tx.send(AppEvent::StreamDone);
            })?;
    }

    let _ = tx.send(AppEvent::StreamDone);
    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::orchestrator::{IntentType, PlanStep};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── build_synthesis_context ──────────────────────────────────────────────

    #[test]
    fn build_synthesis_context_empty_returns_empty_string() {
        assert_eq!(build_synthesis_context(&[]), "");
    }

    #[test]
    fn build_synthesis_context_tags_search_output() {
        let outputs = vec![(AgentKind::Search, "Paris is the capital.".into())];
        let result = build_synthesis_context(&outputs);
        assert!(
            result.starts_with("[SEARCH RESULT]"),
            "should start with search tag, got: {result:?}"
        );
        assert!(
            result.contains("Paris is the capital."),
            "should include the output text, got: {result:?}"
        );
    }

    #[test]
    fn build_synthesis_context_tags_code_output() {
        let outputs = vec![(AgentKind::Code, "Build passed.".into())];
        let result = build_synthesis_context(&outputs);
        assert!(result.starts_with("[CODE OUTPUT]"), "got: {result:?}");
        assert!(result.contains("Build passed."), "got: {result:?}");
    }

    #[test]
    fn build_synthesis_context_multiple_outputs_separated_by_blank_line() {
        let outputs = vec![
            (AgentKind::Search, "search text".into()),
            (AgentKind::Code, "code text".into()),
        ];
        let result = build_synthesis_context(&outputs);
        assert!(
            result.contains("\n\n"),
            "multiple outputs must be separated by a blank line, got: {result:?}"
        );
        let search_pos = result.find("[SEARCH RESULT]").unwrap();
        let code_pos = result.find("[CODE OUTPUT]").unwrap();
        assert!(
            search_pos < code_pos,
            "search result should appear before code output"
        );
    }

    // ── make_filtering_tx ────────────────────────────────────────────────────

    #[tokio::test]
    async fn make_filtering_tx_drops_token_events() {
        let (real_tx, mut real_rx) = mpsc::unbounded_channel::<AppEvent>();
        let filter_tx = make_filtering_tx(real_tx);

        filter_tx.send(AppEvent::Token("raw token".into())).unwrap();
        // Drop the sender so the forwarding task drains and exits.
        drop(filter_tx);
        // Give the spawned task a moment to run.
        tokio::task::yield_now().await;

        let mut saw_token = false;
        while let Ok(event) = real_rx.try_recv() {
            if matches!(event, AppEvent::Token(_)) {
                saw_token = true;
            }
        }
        assert!(
            !saw_token,
            "Token events must be suppressed by the filtering tx"
        );
    }

    #[tokio::test]
    async fn make_filtering_tx_forwards_tool_call_events() {
        let (real_tx, mut real_rx) = mpsc::unbounded_channel::<AppEvent>();
        let filter_tx = make_filtering_tx(real_tx);

        filter_tx
            .send(AppEvent::ToolCall {
                name: "web_search".into(),
                args: "{}".into(),
            })
            .unwrap();
        drop(filter_tx);
        tokio::task::yield_now().await;

        let events: Vec<_> = std::iter::from_fn(|| real_rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AppEvent::ToolCall { name, .. } if name == "web_search")),
            "ToolCall events must pass through the filtering tx"
        );
    }

    fn ndjson(chunks: &[(&str, bool)]) -> String {
        chunks
            .iter()
            .map(|(t, d)| format!("{{\"message\":{{\"content\":\"{t}\"}},\"done\":{d}}}\n"))
            .collect()
    }

    fn chat_plan(task: &str) -> TaskPlan {
        TaskPlan {
            intent_type: IntentType::Conversational,
            steps: vec![PlanStep {
                agent: AgentKind::Chat,
                task: task.into(),
                depends_on: None,
            }],
        }
    }

    // ── happy-path ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_chat_step_streams_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("hi", true)])))
            .mount(&server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());
        let plan = chat_plan("say hi");

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx)
            .await
            .unwrap();

        let mut tokens = Vec::new();
        let mut saw_done = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::Token(t) => tokens.push(t),
                AppEvent::StreamDone => saw_done = true,
                _ => {}
            }
        }
        assert!(!tokens.is_empty(), "expected tokens");
        assert!(saw_done, "expected StreamDone after all steps");
    }

    #[tokio::test]
    async fn empty_steps_sends_stream_done() {
        let plan = TaskPlan {
            intent_type: IntentType::Conversational,
            steps: vec![],
        };
        let (tx, mut rx) = mpsc::unbounded_channel();

        run_plan(
            &plan,
            &[],
            "m",
            "http://unused",
            std::path::Path::new("."),
            tx,
        )
        .await
        .unwrap();

        let mut saw_done = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AppEvent::StreamDone) {
                saw_done = true;
            }
        }
        assert!(saw_done, "StreamDone should be sent even for empty plans");
    }

    #[tokio::test]
    async fn unknown_specialist_falls_back_to_chat() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("fallback", true)])))
            .mount(&server)
            .await;

        let plan = TaskPlan {
            intent_type: IntentType::Conversational,
            steps: vec![PlanStep {
                agent: AgentKind::Unknown,
                task: "answer".into(),
                depends_on: None,
            }],
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx)
            .await
            .unwrap();

        let tokens: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| {
                if let AppEvent::Token(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        assert!(!tokens.is_empty(), "Unknown should fall back to Chat");
    }

    // ── depends_on ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn two_steps_with_depends_on_both_called() {
        let server = MockServer::start().await;
        // First step response
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("step1", true)])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second step response
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("step2", true)])))
            .mount(&server)
            .await;

        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![
                PlanStep {
                    agent: AgentKind::Chat,
                    task: "step1".into(),
                    depends_on: None,
                },
                PlanStep {
                    agent: AgentKind::Chat,
                    task: "step2".into(),
                    depends_on: Some(0),
                },
            ],
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx)
            .await
            .unwrap();

        let tokens: String = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| {
                if let AppEvent::Token(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        assert!(tokens.contains("step1") && tokens.contains("step2"));
    }

    // ── concurrency_safe ─────────────────────────────────────────────────────

    #[test]
    fn concurrency_safe_flags_are_correct() {
        assert!(
            chat::ChatSpecialist::concurrency_safe(),
            "Chat should be concurrency-safe"
        );
        assert!(
            search::SearchSpecialist::concurrency_safe(),
            "Search should be concurrency-safe"
        );
        assert!(
            !code::CodeSpecialist::concurrency_safe(),
            "Code should NOT be concurrency-safe"
        );
    }

    // ── sad-path ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn connection_error_returns_err() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let (tx, _rx) = mpsc::unbounded_channel();
        let url = format!("http://127.0.0.1:{port}/api/chat");
        let plan = chat_plan("task");

        let result = run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx).await;
        assert!(result.is_err());
    }
}
