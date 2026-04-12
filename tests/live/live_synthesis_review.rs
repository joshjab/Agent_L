//! Manual-review live tests for the Persona Synthesis Layer (M7.8).
//!
//! Print responses so a human can verify they sound like Agent-L, not raw
//! specialist output. Automated assertions check only the mechanism.
//!
//! Run this category:
//! ```bash
//! cargo test --test live live_synthesis_review:: -- --ignored --nocapture
//! ```

use agent_l::agents::orchestrator::{AgentKind, IntentType, PlanStep, TaskPlan};
use agent_l::agents::specialists::run_plan;
use agent_l::app::AppEvent;
use agent_l::config::Config;
use tokio::sync::mpsc;

fn live_config() -> Config {
    let _ = dotenvy::dotenv();
    Config::from_env()
}

fn chat_url(cfg: &Config) -> String {
    format!("{}/api/chat", cfg.base_url)
}

fn model(cfg: &Config) -> &str {
    &cfg.model_name
}

fn search_plan(task: &str) -> TaskPlan {
    TaskPlan {
        intent_type: IntentType::Factual,
        steps: vec![PlanStep {
            agent: AgentKind::Search,
            task: task.to_string(),
            depends_on: None,
        }],
    }
}

/// Run a factual question through the full pipeline (Search + synthesis) and
/// return the Token stream as a single string.
async fn run_synthesis_query(question: &str) -> String {
    let cfg = live_config();
    let (tx, mut rx) = mpsc::unbounded_channel();
    run_plan(
        &search_plan(question),
        &[],
        model(&cfg),
        &chat_url(&cfg),
        std::path::Path::new("."),
        tx,
    )
    .await
    .expect("synthesis query should not error");

    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|e| {
            if let AppEvent::Token(t) = e {
                Some(t)
            } else {
                None
            }
        })
        .collect()
}

fn print_review(label: &str, question: &str, response: &str) {
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("REVIEW REQUIRED — {label}");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("QUERY:    {question}");
    println!("RESPONSE:");
    println!("{response}");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Synthesis must not leak the raw "FinalAnswer:" prefix that the Search
/// specialist uses internally. The user should see Agent-L's voice, not the
/// ReAct loop's output format.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn review_synthesis_voice_for_factual_query() {
    let question = "Who is the current Prime Minister of Canada?";
    let response = run_synthesis_query(question).await;

    print_review("verify no raw specialist format", question, &response);

    assert!(!response.is_empty(), "response must not be empty");
    assert!(
        !response.contains("FinalAnswer:"),
        "raw 'FinalAnswer:' must not reach the UI — synthesis should rewrite it. \
         Got: {response:?}"
    );
}

/// The synthesis model must not echo the source tag back to the user.
/// "[SEARCH RESULT]" is internal plumbing; Agent-L's response should read
/// naturally without it.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn review_synthesis_voice_no_raw_tags() {
    let question = "What is the current price of gold per ounce?";
    let response = run_synthesis_query(question).await;

    print_review("verify no raw source tags", question, &response);

    assert!(!response.is_empty(), "response must not be empty");
    assert!(
        !response.contains("[SEARCH RESULT]"),
        "'[SEARCH RESULT]' tag must not appear in the final response. \
         Got: {response:?}"
    );
}

/// Run two different factual queries and print both side-by-side so the reviewer
/// can manually check that the tone is consistent across queries — both should
/// sound like the same Agent-L persona.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn review_synthesis_consistent_voice() {
    let q1 = "Who is the current president of France?";
    let q2 = "What is the population of Tokyo?";

    let r1 = run_synthesis_query(q1).await;
    let r2 = run_synthesis_query(q2).await;

    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("REVIEW REQUIRED — check both responses share Agent-L's tone");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("QUERY 1: {q1}");
    println!("RESPONSE 1:\n{r1}");
    println!();
    println!("QUERY 2: {q2}");
    println!("RESPONSE 2:\n{r2}");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    assert!(!r1.is_empty(), "first response must not be empty");
    assert!(!r2.is_empty(), "second response must not be empty");
    assert!(
        !r1.contains("FinalAnswer:"),
        "r1 must not contain raw FinalAnswer"
    );
    assert!(
        !r2.contains("FinalAnswer:"),
        "r2 must not contain raw FinalAnswer"
    );
}
