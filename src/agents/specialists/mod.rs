pub mod chat;
pub mod code;

use std::path::Path;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::agents::orchestrator::{AgentKind, TaskPlan};

use chat::ChatSpecialist;

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
/// Unknown specialist kinds fall back to `ChatSpecialist`.
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

    for step in &plan.steps {
        // Resolve context from a prior step if requested.
        let context: Option<&str> = step
            .depends_on
            .and_then(|idx| step_outputs.get(idx))
            .and_then(|opt| opt.as_deref());

        let output = match step.agent {
            // Implemented: streams tokens to the UI.
            AgentKind::Chat | AgentKind::Unknown => {
                had_streaming_step = true;
                ChatSpecialist
                    .run(&step.task, messages, context, model, chat_url, tx.clone())
                    .await
                    .map_err(|e| {
                        let _ = tx.send(AppEvent::StreamDone);
                        e
                    })?
            }
            AgentKind::Code => {
                had_streaming_step = true;
                let specialist =
                    code::CodeSpecialist::new(model, chat_url, working_dir);
                let output = specialist
                    .run(&step.task, tx.clone())
                    .await
                    .map_err(|msg| {
                        let _ = tx.send(AppEvent::StreamDone);
                        SpecialistError { message: msg }
                    })?;
                // For one-off scope, `run()` returns the full output string;
                // stream it as tokens so the user sees it in the chat.
                if !output.is_empty() {
                    let _ = tx.send(AppEvent::Token(output.clone()));
                }
                output
            }
            // Not yet implemented — return a silent placeholder so `depends_on`
            // chains still get *something*, but don't stream duplicate responses.
            AgentKind::Search
            | AgentKind::Shell
            | AgentKind::Calendar
            | AgentKind::Memory => {
                format!("[{:?} specialist not yet implemented]", step.agent)
            }
        };

        step_outputs.push(Some(output));
    }

    // If no step streamed a response (e.g. a plan with only Search steps
    // before those specialists are built), fall back to a single Chat call so
    // the user always gets an answer.
    if !had_streaming_step && !plan.steps.is_empty() {
        let fallback_task = plan.steps[0].task.as_str();
        ChatSpecialist
            .run(fallback_task, messages, None, model, chat_url, tx.clone())
            .await
            .map_err(|e| {
                let _ = tx.send(AppEvent::StreamDone);
                e
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
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            .respond_with(
                ResponseTemplate::new(200).set_body_string(ndjson(&[("hi", true)])),
            )
            .mount(&server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());
        let plan = chat_plan("say hi");

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx).await.unwrap();

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

        run_plan(&plan, &[], "m", "http://unused", std::path::Path::new("."), tx).await.unwrap();

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
            .respond_with(
                ResponseTemplate::new(200).set_body_string(ndjson(&[("fallback", true)])),
            )
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

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx).await.unwrap();

        let tokens: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| if let AppEvent::Token(t) = e { Some(t) } else { None })
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
            .respond_with(
                ResponseTemplate::new(200).set_body_string(ndjson(&[("step1", true)])),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second step response
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(ndjson(&[("step2", true)])),
            )
            .mount(&server)
            .await;

        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![
                PlanStep { agent: AgentKind::Chat, task: "step1".into(), depends_on: None },
                PlanStep { agent: AgentKind::Chat, task: "step2".into(), depends_on: Some(0) },
            ],
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx).await.unwrap();

        let tokens: String = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| if let AppEvent::Token(t) = e { Some(t) } else { None })
            .collect();
        assert!(tokens.contains("step1") && tokens.contains("step2"));
    }

    // ── unimplemented specialists ────────────────────────────────────────────

    #[tokio::test]
    async fn search_only_plan_falls_back_to_chat_once() {
        // Agent L returns [Search] for a factual query. Since Search isn't
        // implemented yet, run_plan must still produce exactly ONE streaming
        // response (the Chat fallback) — not zero, not three.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(ndjson(&[("answer", true)])),
            )
            .mount(&server)
            .await;

        let plan = TaskPlan {
            intent_type: IntentType::Factual,
            steps: vec![PlanStep {
                agent: AgentKind::Search,
                task: "look up capital of France".into(),
                depends_on: None,
            }],
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx).await.unwrap();

        let tokens: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| if let AppEvent::Token(t) = e { Some(t) } else { None })
            .collect();
        assert!(!tokens.is_empty(), "expected a fallback Chat response");
        // Only one HTTP call should have been made (the fallback Chat call).
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn multi_step_with_only_unimplemented_sends_one_response() {
        // A plan like [Search, Shell] should produce exactly one response via
        // the Chat fallback, not two (one per step).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(ndjson(&[("one", true)])),
            )
            .mount(&server)
            .await;

        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![
                PlanStep { agent: AgentKind::Search, task: "search".into(), depends_on: None },
                PlanStep { agent: AgentKind::Shell, task: "run".into(), depends_on: Some(0) },
            ],
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());

        run_plan(&plan, &[], "m", &url, std::path::Path::new("."), tx).await.unwrap();

        let tokens: String = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| if let AppEvent::Token(t) = e { Some(t) } else { None })
            .collect();
        assert_eq!(tokens, "one", "expected exactly one Chat fallback response");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
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
