//! Live integration tests that require a running Ollama instance.
//!
//! All tests are `#[ignore]` by default so that `cargo test` stays fast.
//! To run them:
//!
//! ```bash
//! cargo test --test live_pipeline -- --ignored --nocapture
//! ```
//!
//! Prerequisites:
//! - Ollama running at `OLLAMA_HOST:OLLAMA_PORT` (defaults: `localhost:11434`)
//! - The configured model pulled locally (`OLLAMA_MODEL`, default: `llama3.2`)
//!
//! See `tests/live/README.md` for full setup instructions.

use agent_l::agents::orchestrator::{AgentKind, IntentType, OrchestratorAgent, PlanStep, TaskPlan};
use agent_l::agents::specialists::run_plan;
use agent_l::app::AppEvent;
use agent_l::config::Config;
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

// ─── Chat specialist ─────────────────────────────────────────────────────────

/// Conversational queries must produce a non-empty response via the Chat specialist.
#[tokio::test]
#[ignore]
async fn live_conversational_produces_response() {
    let cfg = live_config();
    let plan = TaskPlan {
        intent_type: IntentType::Conversational,
        steps: vec![PlanStep {
            agent: AgentKind::Chat,
            task: "say the word 'hello' and nothing else".into(),
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
    .expect("live conversational query should succeed");

    let tokens: String = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|e| {
            if let AppEvent::Token(t) = e {
                Some(t)
            } else {
                None
            }
        })
        .collect();

    assert!(!tokens.is_empty(), "Chat specialist should produce tokens");
    assert!(
        tokens.to_lowercase().contains("hello"),
        "Response should contain 'hello', got: {tokens:?}"
    );
}

// ─── Search specialist ───────────────────────────────────────────────────────

/// Factual queries must route to Search and return a sensible answer.
#[tokio::test]
#[ignore]
async fn live_factual_returns_cited_answer() {
    let cfg = live_config();
    let plan = TaskPlan {
        intent_type: IntentType::Factual,
        steps: vec![PlanStep {
            agent: AgentKind::Search,
            task: "what country is Paris the capital of? reply in one word".into(),
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
    .expect("live factual query should succeed");

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
        tokens.to_lowercase().contains("france"),
        "Factual answer should mention France, got: {tokens:?}"
    );
}

/// Search answer must not contain the same sentence twice (regression for M7 dedup bug).
#[tokio::test]
#[ignore]
async fn live_search_does_not_duplicate_sentences() {
    let cfg = live_config();
    let plan = TaskPlan {
        intent_type: IntentType::Factual,
        steps: vec![PlanStep {
            agent: AgentKind::Search,
            task: "in one sentence, what is the capital of France?".into(),
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
    .expect("live search should succeed");

    let tokens: String = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|e| {
            if let AppEvent::Token(t) = e {
                Some(t)
            } else {
                None
            }
        })
        .collect();

    // Check that no sentence appears more than once (case-insensitive, split on ". ")
    let sentences: Vec<&str> = tokens.split(". ").map(str::trim).collect();
    let unique: std::collections::HashSet<String> = sentences
        .iter()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let non_empty_count = sentences.iter().filter(|s| !s.is_empty()).count();

    assert_eq!(
        non_empty_count,
        unique.len(),
        "Search answer should not contain duplicate sentences, got: {tokens:?}"
    );
}

// ─── Agent L routing ─────────────────────────────────────────────────────────

/// Agent L must classify a simple math question as Conversational, not Factual.
#[tokio::test]
#[ignore]
async fn live_agent_l_routes_math_to_conversational() {
    use agent_l::agents::call_with_retry;
    use serde_json::json;

    let cfg = live_config();
    let agent = OrchestratorAgent::new(model(&cfg));
    let ctx = vec![json!({"role": "user", "content": "what is 2+2?"})];
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

    assert!(
        matches!(
            plan.intent_type,
            IntentType::Conversational | IntentType::Creative
        ),
        "2+2 should not be Factual or Task, got: {:?}",
        plan.intent_type
    );
}

/// Agent L must route file-search queries to Search, not Code (regression for M7.5 routing bug).
#[tokio::test]
#[ignore]
async fn live_file_search_routes_to_search_not_code() {
    use agent_l::agents::call_with_retry;
    use serde_json::json;

    let cfg = live_config();
    let agent = OrchestratorAgent::new(model(&cfg));
    let ctx = vec![json!({"role": "user", "content": "search my project files for 'retry'"})];
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

    assert_eq!(
        plan.steps[0].agent,
        AgentKind::Search,
        "file search should route to Search, got: {:?}",
        plan.steps[0].agent
    );
}
