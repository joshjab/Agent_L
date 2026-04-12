//! Integration tests for the Persona Synthesis Layer (M7.8).
//!
//! These tests verify that non-Chat specialist outputs are routed through the
//! synthesis step and that raw specialist tokens do not reach the UI.
//!
//! DDG is mocked via `AGENT_L_DDG_BASE_URL`. All env-var tests share a static
//! mutex to prevent parallel races (same pattern as `config.rs`).

use agent_l::agents::orchestrator::{AgentKind, IntentType, PlanStep, TaskPlan};
use agent_l::agents::specialists::run_plan;
use agent_l::app::AppEvent;
use serde_json::json;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Serialise all env-var manipulation to prevent parallel test races.
// Use unwrap_or_else to recover from a poisoned mutex (caused by a prior
// test panic) so one failure doesn't cascade to all subsequent tests.
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Ollama non-streaming response envelope (used by the Search ReAct loop).
fn ollama_nonstream(content: &str) -> String {
    json!({
        "model": "m",
        "message": {"role": "assistant", "content": content},
        "done": true
    })
    .to_string()
}

/// Ollama streaming NDJSON response (used by ChatSpecialist / synthesis).
fn ollama_stream(tokens: &[(&str, bool)]) -> String {
    tokens
        .iter()
        .map(|(t, done)| format!("{{\"message\":{{\"content\":\"{t}\"}},\"done\":{done}}}\n"))
        .collect()
}

/// DuckDuckGo instant-answer response with a long-enough snippet to exceed the
/// synthesis threshold when combined with the FinalAnswer text.
fn ddg_resp_long(abstract_text: &str, url: &str) -> String {
    format!(
        r#"{{"AbstractText":"{abstract_text}","AbstractURL":"{url}","AbstractSource":"Wikipedia","RelatedTopics":[]}}"#
    )
}

/// DuckDuckGo instant-answer response whose AbstractText is short enough that
/// the combined FinalAnswer stays under SYNTHESIS_MIN_CHARS (200 chars).
fn ddg_resp_short() -> String {
    r#"{"AbstractText":"Short.","AbstractURL":"https://example.com","AbstractSource":"Test","RelatedTopics":[]}"#.into()
}

fn search_plan(task: &str) -> TaskPlan {
    TaskPlan {
        intent_type: IntentType::Factual,
        steps: vec![PlanStep {
            agent: AgentKind::Search,
            task: task.into(),
            depends_on: None,
        }],
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

/// A Search step followed by synthesis must make exactly 3 Ollama calls:
/// 1. Search ReAct — tool call
/// 2. Search ReAct — FinalAnswer
/// 3. Synthesis — ChatSpecialist streaming call
#[tokio::test(flavor = "multi_thread")]
async fn search_result_goes_through_synthesis() {
    let _guard = lock_env();

    let ollama = MockServer::start().await;
    let ddg = MockServer::start().await;

    // Search step 1: model issues a tool call.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_nonstream(
            "Thought: need to search\nToolCall: web_search {\"query\":\"capital of France\"}",
        )))
        .up_to_n_times(1)
        .mount(&ollama)
        .await;

    // DDG returns a long result (ensures combined output > SYNTHESIS_MIN_CHARS).
    Mock::given(method("GET"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ddg_resp_long(
            "Paris is the capital and most populous city of France. \
             It is located along the Seine River in northern France and is known \
             worldwide as the City of Light.",
            "https://en.wikipedia.org/wiki/France",
        )))
        .mount(&ddg)
        .await;

    // Search step 2: model gives the FinalAnswer — long enough (> 200 chars
    // after the "[SEARCH RESULT]\n" tag is prepended) to trigger synthesis.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_nonstream(
            "FinalAnswer: The capital of France is Paris, widely known as the City \
             of Light. It is the largest city in France, situated along the Seine \
             River in the north of the country. Paris has a population of over two \
             million people in the city proper and is a major European hub for \
             culture, fashion, gastronomy, and the arts.",
        )))
        .up_to_n_times(1)
        .mount(&ollama)
        .await;

    // Synthesis step: streaming response.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_stream(&[
            ("Paris", false),
            (" is France's capital — the City of Light.", true),
        ])))
        .mount(&ollama)
        .await;

    unsafe { std::env::set_var("AGENT_L_DDG_BASE_URL", ddg.uri()) };

    let (tx, mut rx) = mpsc::unbounded_channel();
    run_plan(
        &search_plan("What is the capital of France?"),
        &[],
        "m",
        &format!("{}/api/chat", ollama.uri()),
        std::path::Path::new("."),
        tx,
    )
    .await
    .unwrap();

    unsafe { std::env::remove_var("AGENT_L_DDG_BASE_URL") };

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let saw_done = events.iter().any(|e| matches!(e, AppEvent::StreamDone));
    assert!(saw_done, "StreamDone must be sent");

    // Exactly 3 Ollama POST calls.
    let reqs = ollama.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "expected 3 Ollama calls (2 search ReAct + 1 synthesis), got {}",
        reqs.len()
    );
}

/// Even a short search FinalAnswer must go through synthesis — synthesis always
/// fires so source tags never leak to the user and voice stays consistent.
/// Expect 3 Ollama calls: 2 for the ReAct loop, 1 for synthesis.
#[tokio::test(flavor = "multi_thread")]
async fn short_search_output_still_synthesizes() {
    let _guard = lock_env();

    let ollama = MockServer::start().await;
    let ddg = MockServer::start().await;

    // Search step 1: tool call.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_nonstream(
            "Thought: search\nToolCall: web_search {\"query\":\"test\"}",
        )))
        .up_to_n_times(1)
        .mount(&ollama)
        .await;

    // DDG returns a short result.
    Mock::given(method("GET"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ddg_resp_short()))
        .mount(&ddg)
        .await;

    // Search step 2: short FinalAnswer.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(ollama_nonstream("FinalAnswer: Short.")),
        )
        .up_to_n_times(1)
        .mount(&ollama)
        .await;

    // Synthesis step.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(ollama_stream(&[("Short answer.", true)])),
        )
        .mount(&ollama)
        .await;

    unsafe { std::env::set_var("AGENT_L_DDG_BASE_URL", ddg.uri()) };

    let (tx, mut rx) = mpsc::unbounded_channel();
    run_plan(
        &search_plan("short query"),
        &[],
        "m",
        &format!("{}/api/chat", ollama.uri()),
        std::path::Path::new("."),
        tx,
    )
    .await
    .unwrap();

    unsafe { std::env::remove_var("AGENT_L_DDG_BASE_URL") };

    let _events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    // 3 Ollama calls — synthesis always runs.
    let reqs = ollama.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "synthesis should always fire (expected 3 Ollama calls), got {}",
        reqs.len()
    );
}

/// Raw "FinalAnswer: ..." text from the search specialist must NOT appear in the
/// Token events received by the app — synthesis re-streams in Agent-L's voice.
#[tokio::test(flavor = "multi_thread")]
async fn raw_search_tokens_do_not_reach_ui() {
    let _guard = lock_env();

    let ollama = MockServer::start().await;
    let ddg = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_nonstream(
            "Thought: search\nToolCall: web_search {\"query\":\"president\"}",
        )))
        .up_to_n_times(1)
        .mount(&ollama)
        .await;

    Mock::given(method("GET"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ddg_resp_long(
            "Donald Trump is the 47th President of the United States, \
             having taken office on January 20 2025 after winning the 2024 \
             presidential election against Kamala Harris.",
            "https://www.whitehouse.gov",
        )))
        .mount(&ddg)
        .await;

    // Search FinalAnswer — long enough to trigger synthesis, and contains the
    // sentinel string we will assert does NOT reach the UI.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_nonstream(
            "FinalAnswer: SENTINEL_RAW_SEARCH_TEXT Trump is the 47th President of \
             the United States, having taken office on January 20 2025 after winning \
             the 2024 presidential election. He previously served as the 45th \
             President from 2017 to 2021.",
        )))
        .up_to_n_times(1)
        .mount(&ollama)
        .await;

    // Synthesis streams something different.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ollama_stream(&[(
            "The current US president is Trump.",
            true,
        )])))
        .mount(&ollama)
        .await;

    unsafe { std::env::set_var("AGENT_L_DDG_BASE_URL", ddg.uri()) };

    let (tx, mut rx) = mpsc::unbounded_channel();
    run_plan(
        &search_plan("Who is the US president?"),
        &[],
        "m",
        &format!("{}/api/chat", ollama.uri()),
        std::path::Path::new("."),
        tx,
    )
    .await
    .unwrap();

    unsafe { std::env::remove_var("AGENT_L_DDG_BASE_URL") };

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
        !tokens.contains("SENTINEL_RAW_SEARCH_TEXT"),
        "raw search FinalAnswer must not reach the UI, got tokens: {tokens:?}"
    );
    assert!(
        tokens.contains("Trump"),
        "synthesis should still include the answer, got tokens: {tokens:?}"
    );
}
