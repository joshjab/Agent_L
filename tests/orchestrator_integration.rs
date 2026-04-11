use agent_l::agents::{
    call_with_retry,
    orchestrator::{AgentKind, IntentType, OrchestratorAgent},
};
use agent_l::ollama::post_json;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Wrap a TaskPlan JSON value as the envelope Ollama returns for a non-streaming call.
fn ollama_envelope(plan: serde_json::Value) -> serde_json::Value {
    json!({ "message": { "role": "assistant", "content": plan.to_string() } })
}

fn single_step(intent: &str, agent: &str) -> serde_json::Value {
    json!({
        "intent_type": intent,
        "steps": [{ "agent": agent, "task": "handle it" }]
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn run(
    server: &MockServer,
    context: serde_json::Value,
) -> Result<agent_l::agents::orchestrator::TaskPlan, agent_l::agents::AgentError> {
    let agent = OrchestratorAgent::new("test-model");
    let url = format!("{}/api/chat", server.uri());
    let ctx = vec![context];
    call_with_retry(
        &agent,
        &ctx,
        |req| {
            let url = url.clone();
            async move { post_json(&url, req).await }
        },
        3,
    )
    .await
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// A factual question ("what is X?") should route to Search with Factual intent.
#[tokio::test]
async fn factual_intent_routes_to_search() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ollama_envelope(single_step("Factual", "Search"))),
        )
        .mount(&server)
        .await;

    let plan = run(
        &server,
        json!({"role": "user", "content": "what is the capital of France?"}),
    )
    .await
    .unwrap();

    assert_eq!(plan.intent_type, IntentType::Factual);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].agent, AgentKind::Search);
}

/// A greeting should route to Chat with Conversational intent.
#[tokio::test]
async fn conversational_intent_routes_to_chat() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ollama_envelope(single_step("Conversational", "Chat"))),
        )
        .mount(&server)
        .await;

    let plan = run(&server, json!({"role": "user", "content": "hello there!"}))
        .await
        .unwrap();

    assert_eq!(plan.intent_type, IntentType::Conversational);
    assert_eq!(plan.steps[0].agent, AgentKind::Chat);
}

/// A multi-step Task plan with a `depends_on` link is parsed and validated correctly.
#[tokio::test]
async fn multistep_task_with_depends_on() {
    let server = MockServer::start().await;
    let plan_json = json!({
        "intent_type": "Task",
        "steps": [
            { "agent": "Search", "task": "find Rust news" },
            { "agent": "Chat",   "task": "summarise results", "depends_on": 0 }
        ]
    });
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_envelope(plan_json)))
        .mount(&server)
        .await;

    let plan = run(
        &server,
        json!({"role": "user", "content": "search for Rust news and summarise it"}),
    )
    .await
    .unwrap();

    assert_eq!(plan.intent_type, IntentType::Task);
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].agent, AgentKind::Search);
    assert_eq!(plan.steps[1].agent, AgentKind::Chat);
    assert_eq!(plan.steps[1].depends_on, Some(0));
}

/// When the model cannot determine a specialist it returns Unknown; that should
/// parse successfully and be returned as-is so the caller can handle it.
#[tokio::test]
async fn unknown_intent_fallback_returns_plan() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ollama_envelope(single_step("Conversational", "Unknown"))),
        )
        .mount(&server)
        .await;

    let plan = run(&server, json!({"role": "user", "content": "¯\\_(ツ)_/¯"}))
        .await
        .unwrap();

    assert_eq!(plan.steps[0].agent, AgentKind::Unknown);
}

/// If the model always returns more than 5 steps the retry loop exhausts all
/// attempts and surfaces an `AgentError` rather than panicking or looping forever.
#[tokio::test]
async fn over_five_steps_exhausts_retries_and_returns_error() {
    let server = MockServer::start().await;

    // 6 steps — always fails TaskPlan::validate()
    let bad_plan = json!({
        "intent_type": "Task",
        "steps": [
            { "agent": "Chat", "task": "1" },
            { "agent": "Chat", "task": "2" },
            { "agent": "Chat", "task": "3" },
            { "agent": "Chat", "task": "4" },
            { "agent": "Chat", "task": "5" },
            { "agent": "Chat", "task": "6" }
        ]
    });
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_envelope(bad_plan)))
        .mount(&server)
        .await;

    let err = run(&server, json!({"role": "user", "content": "do everything"}))
        .await
        .unwrap_err();

    assert_eq!(err.attempts, 3, "all 3 attempts should be exhausted");
    assert!(
        err.last_error.contains("maximum"),
        "error should mention the step limit: {}",
        err.last_error
    );

    // Verify the server saw exactly 3 requests (one per attempt)
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 3);
}

/// On a parse failure the retry prompt contains the error text so the model
/// can self-correct. Verify the second request body includes the error.
#[tokio::test]
async fn retry_prompt_embeds_error_from_previous_attempt() {
    let server = MockServer::start().await;

    // First response: invalid JSON content (triggers parse failure + retry)
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "message": { "content": "not valid json at all" } })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second response: a valid plan
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ollama_envelope(single_step("Conversational", "Chat"))),
        )
        .mount(&server)
        .await;

    let plan = run(&server, json!({"role": "user", "content": "hello"}))
        .await
        .unwrap();

    assert_eq!(plan.steps[0].agent, AgentKind::Chat);

    // The second request should contain the parse error in its body
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "should have made exactly 2 requests");
    let second_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let messages = second_body["messages"].as_array().unwrap();
    let last_msg = messages.last().unwrap();
    assert_eq!(last_msg["role"], "user");
    let content = last_msg["content"].as_str().unwrap();
    assert!(
        content.contains("invalid"),
        "retry message should say the prior response was invalid: {content}"
    );
}
