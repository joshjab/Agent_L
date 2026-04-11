//! Manual-review live tests for factual accuracy.
//!
//! These tests run a factual question through the full pipeline and **print
//! the response** so a human can verify correctness before the commit lands.
//! They assert only the *mechanism* (non-empty response, web_search was called)
//! — not the specific answer, which changes over time and can't be hard-coded.
//!
//! ## How to run
//!
//! ```bash
//! cargo test --test live_factual_review -- --ignored --nocapture
//! ```
//!
//! Read the printed answers before pressing Enter in the pre-commit prompt.
//!
//! ## Prerequisites
//! - Ollama running at `OLLAMA_HOST:OLLAMA_PORT` (defaults: `localhost:11434`)
//! - The configured model pulled locally (`OLLAMA_MODEL`, default: `llama3.2`)

use agent_l::agents::orchestrator::{AgentKind, IntentType, OrchestratorAgent, PlanStep, TaskPlan};
use agent_l::agents::specialists::run_plan;
use agent_l::app::AppEvent;
use agent_l::config::Config;
use serde_json::json;
use tokio::sync::mpsc;

fn live_config() -> Config {
    Config::from_env()
}

fn chat_url(cfg: &Config) -> String {
    format!("{}/api/chat", cfg.base_url)
}

fn model(cfg: &Config) -> &str {
    &cfg.model_name
}

/// Run a factual question through the Search specialist and print the response.
/// Returns `(response_text, tool_calls)` for the minimal assertions each test makes.
async fn run_factual_review(question: &str) -> (String, Vec<String>) {
    let cfg = live_config();
    let plan = TaskPlan {
        intent_type: IntentType::Factual,
        steps: vec![PlanStep {
            agent: AgentKind::Search,
            task: question.to_string(),
            depends_on: None,
        }],
    };

    let (tx, mut rx) = mpsc::unbounded_channel();
    run_plan(
        &plan,
        &[],
        model(&cfg),
        &chat_url(&cfg),
        std::path::Path::new("."),
        tx,
    )
    .await
    .expect("factual review query should not return an error");

    let mut response = String::new();
    let mut tool_calls: Vec<String> = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::Token(t) => response.push_str(&t),
            AppEvent::ToolCall { name, .. } => tool_calls.push(name),
            _ => {}
        }
    }
    (response, tool_calls)
}

/// Print a clearly formatted review block so the human can easily read it.
fn print_review(question: &str, response: &str, tool_calls: &[String]) {
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("REVIEW REQUIRED — verify this answer is factually correct");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("QUERY:      {question}");
    println!("TOOLS USED: {}", tool_calls.join(", "));
    println!("RESPONSE:");
    println!("{response}");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// "Who is the current president of the United States?" must route to Search,
/// call web_search, and return a non-empty answer. Human verifies correctness.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn review_current_us_president() {
    let question = "Who is the current president of the United States?";
    let (response, tool_calls) = run_factual_review(question).await;

    print_review(question, &response, &tool_calls);

    assert!(
        tool_calls.iter().any(|n| n == "web_search"),
        "Must call web_search before answering, tool_calls: {tool_calls:?}"
    );
    assert!(!response.is_empty(), "Response must not be empty");
    // The model must NOT have answered from knowledge before searching.
    // If no URL appears in the response, it likely fabricated the answer.
    assert!(
        response.contains("http://") || response.contains("https://"),
        "Response must cite a source URL from the search result, got: {response:?}"
    );
}

/// "Who is the current Prime Minister of the United Kingdom?" — another
/// current-leader question that models often answer from stale training data.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn review_current_uk_pm() {
    let question = "Who is the current Prime Minister of the United Kingdom?";
    let (response, tool_calls) = run_factual_review(question).await;

    print_review(question, &response, &tool_calls);

    assert!(
        tool_calls.iter().any(|n| n == "web_search"),
        "Must call web_search, tool_calls: {tool_calls:?}"
    );
    assert!(!response.is_empty(), "Response must not be empty");
}

/// Verify that the orchestrator correctly routes "current president" to Search
/// (not Chat) — regression guard for the routing bug described in the incident.
#[tokio::test]
#[ignore]
async fn review_routing_current_president_goes_to_search() {
    use agent_l::agents::call_with_retry;

    let cfg = live_config();
    let agent = OrchestratorAgent::new(model(&cfg));
    let ctx = vec![
        json!({"role": "user", "content": "Who is the current president of the United States?"}),
    ];
    let url = chat_url(&cfg);

    let plan = call_with_retry(
        &agent,
        &ctx,
        |req| {
            let url = url.clone();
            async move { agent_l::ollama::post_json(&url, req).await }
        },
        3,
    )
    .await
    .expect("orchestrator should classify successfully");

    println!();
    println!("━━━ ROUTING REVIEW ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("QUERY:       Who is the current president of the United States?");
    println!("INTENT:      {:?}", plan.intent_type);
    println!("FIRST AGENT: {:?}", plan.steps[0].agent);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    assert_eq!(
        plan.intent_type,
        IntentType::Factual,
        "Current-president query must be classified Factual, got: {:?}",
        plan.intent_type
    );
    assert_eq!(
        plan.steps[0].agent,
        AgentKind::Search,
        "Current-president query must route to Search, got: {:?}",
        plan.steps[0].agent
    );
}
