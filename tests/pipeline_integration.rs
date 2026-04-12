use agent_l::agents::orchestrator::{AgentKind, IntentType, PlanStep, TaskPlan};
use agent_l::agents::specialists::run_plan;
use agent_l::app::AppEvent;
use serde_json::json;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ndjson(chunks: &[(&str, bool)]) -> String {
    chunks
        .iter()
        .map(|(t, d)| format!("{{\"message\":{{\"content\":\"{t}\"}},\"done\":{d}}}\n"))
        .collect()
}

fn conversational_plan() -> TaskPlan {
    TaskPlan {
        intent_type: IntentType::Conversational,
        steps: vec![PlanStep {
            agent: AgentKind::Chat,
            task: "respond to the user".into(),
            depends_on: None,
        }],
    }
}

// ── Single-step Conversational → Chat ────────────────────────────────────────

#[tokio::test]
async fn chat_plan_streams_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ndjson(&[("Hello", false), (" there", true)])),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());

    run_plan(
        &conversational_plan(),
        &[],
        "test-model",
        &url,
        std::path::Path::new("."),
        tx,
    )
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
    assert_eq!(tokens, "Hello there");
}

#[tokio::test]
async fn chat_plan_sends_stream_done() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("ok", true)])))
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());

    run_plan(
        &conversational_plan(),
        &[],
        "m",
        &url,
        std::path::Path::new("."),
        tx,
    )
    .await
    .unwrap();

    let saw_done =
        std::iter::from_fn(|| rx.try_recv().ok()).any(|e| matches!(e, AppEvent::StreamDone));
    assert!(saw_done, "run_plan must emit StreamDone after all steps");
}

// ── Persona-wrapped messages are forwarded ────────────────────────────────────

#[tokio::test]
async fn chat_plan_uses_provided_messages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("pong", true)])))
        .mount(&server)
        .await;

    let (tx, _rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    let messages = vec![
        json!({"role": "system", "content": "you are helpful"}),
        json!({"role": "user", "content": "ping"}),
    ];

    run_plan(
        &conversational_plan(),
        &messages,
        "m",
        &url,
        std::path::Path::new("."),
        tx,
    )
    .await
    .unwrap();

    // The request was received by the mock server — verify body had our messages.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert!(
        msgs.iter().any(|m| m["content"] == "ping"),
        "expected 'ping' message in Ollama request"
    );
}

// ── Multi-step plan with depends_on ──────────────────────────────────────────

#[tokio::test]
async fn multistep_plan_with_depends_on_runs_all_steps() {
    let server = MockServer::start().await;

    // First step
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("first", true)])))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second step
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ndjson(&[("second", true)])))
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
    assert!(tokens.contains("first") && tokens.contains("second"));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

// ── Search plan uses SearchSpecialist (not Chat fallback) ────────────────────

// SearchSpecialist calls Ollama in non-streaming mode (envelope format).
// Uses local_search (no network call) so the test works without external
// connections. The executor now requires at least one tool call before
// accepting a FinalAnswer, so the mock simulates the full ReAct flow:
// ToolCall → Observation → FinalAnswer.
#[tokio::test(flavor = "multi_thread")]
async fn search_only_plan_calls_search_specialist() {
    let server = MockServer::start().await;

    // First Ollama call: model calls local_search (no network needed).
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            serde_json::json!({
                "model": "m",
                "message": {"role": "assistant", "content": "ToolCall: local_search {\"query\":\"capital of France\",\"path\":\".\"}"},
                "done": true
            })
            .to_string(),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second Ollama call: model gives the final answer after seeing the
    // local_search observation (which will say "No results found" since
    // the project files don't mention the capital of France — that's fine;
    // the test only checks that SearchSpecialist is used, not the content).
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"model":"m","message":{"role":"assistant","content":"FinalAnswer: Paris"},"done":true}"#,
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Third Ollama call: synthesis ChatSpecialist wraps the result in
    // Agent-L's voice (streaming NDJSON).
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"Paris\"},\"done\":true}\n"),
        )
        .mount(&server)
        .await;

    let plan = TaskPlan {
        intent_type: IntentType::Factual,
        steps: vec![PlanStep {
            agent: AgentKind::Search,
            task: "look up the capital of France".into(),
            depends_on: None,
        }],
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
    assert!(
        tokens.contains("Paris"),
        "expected Search answer, got: {tokens}"
    );
    // 2 SearchSpecialist calls (ToolCall + FinalAnswer) + 1 synthesis call.
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

// ── Unknown specialist falls back to Chat ─────────────────────────────────────

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
            task: "do something".into(),
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
    assert!(
        !tokens.is_empty(),
        "Unknown should fall back to Chat and stream tokens"
    );
}
