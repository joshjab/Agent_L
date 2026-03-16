use agent_l::app::{AppEvent, StartupState};
use agent_l::config::Config;
use agent_l::startup::{run_startup_checks, StartupTimings};

use std::time::Duration;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fast_timings() -> StartupTimings {
    StartupTimings {
        max_connect_retries: 2,
        connect_retry_delay: Duration::from_millis(10),
        load_poll_interval: Duration::from_millis(10),
        load_timeout: Duration::from_millis(50),
    }
}

fn fast_timings_3() -> StartupTimings {
    StartupTimings {
        max_connect_retries: 3,
        ..fast_timings()
    }
}

/// Drain all StartupUpdate events from the channel into a Vec<StartupState>.
fn collect_states(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> Vec<StartupState> {
    let mut states = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::StartupUpdate(state) = event {
            states.push(state);
        }
    }
    states
}

#[tokio::test]
async fn happy_path_model_already_loaded() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(states.contains(&StartupState::Connecting), "expected Connecting: {:?}", states);
    assert!(states.contains(&StartupState::CheckingModel), "expected CheckingModel: {:?}", states);
    assert!(states.contains(&StartupState::LoadingModel), "expected LoadingModel: {:?}", states);
    assert!(states.contains(&StartupState::Ready), "expected Ready: {:?}", states);
}

#[tokio::test]
async fn model_loads_after_polling() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    // First /api/ps call returns empty — register first so it matches first (FIFO)
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": []})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Subsequent /api/ps calls return model loaded — fallback registered second
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(states.last() == Some(&StartupState::Ready), "last state should be Ready: {:?}", states);

    let requests = server.received_requests().await.unwrap();
    let ps_count = requests.iter().filter(|r| r.url.path() == "/api/ps").count();
    assert!(ps_count >= 2, "expected ≥2 /api/ps calls, got {}", ps_count);
}

#[tokio::test]
async fn ollama_unreachable() {
    // Grab a free port then release it so nothing is listening there
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    let last = states.last().expect("expected at least one event");
    assert!(
        matches!(last, StartupState::Failed(msg) if msg.contains("Cannot reach Ollama")),
        "expected Failed with 'Cannot reach Ollama', got {:?}", last
    );
}

#[tokio::test]
async fn model_not_found() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "codellama"}]})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(
        states.iter().any(|s| matches!(s, StartupState::Failed(msg) if msg.contains("ollama pull"))),
        "expected Failed with 'ollama pull': {:?}", states
    );

    // /api/ps should not be called
    let requests = server.received_requests().await.unwrap();
    let ps_count = requests.iter().filter(|r| r.url.path() == "/api/ps").count();
    assert_eq!(ps_count, 0, "/api/ps should not be called when model not found");
}

#[tokio::test]
async fn tags_returns_500() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(
        states.iter().any(|s| matches!(s, StartupState::Failed(_))),
        "expected a Failed state: {:?}", states
    );

    let requests = server.received_requests().await.unwrap();
    let tags_count = requests.iter().filter(|r| r.url.path() == "/api/tags").count();
    assert_eq!(tags_count, 2, "expected exactly 2 /api/tags calls, got {}", tags_count);
}

#[tokio::test]
async fn load_timeout_sends_ready() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    // /api/ps always returns empty — model never loads
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": []})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(
        states.last() == Some(&StartupState::Ready),
        "timeout should send Ready, got {:?}", states
    );
}

#[tokio::test]
async fn tags_invalid_json() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(
        states.iter().any(|s| matches!(s, StartupState::Failed(msg) if msg.contains("Cannot reach Ollama"))),
        "expected Failed after all retries, got {:?}", states
    );
}

#[tokio::test]
async fn connecting_event_on_each_retry() {
    let server = MockServer::start().await;

    // First 2 calls return 500 — register first so they match first (FIFO)
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    // Third call returns 200 with model — fallback registered second
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": [{"name": "llama3"}]})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings_3()).await;

    let states = collect_states(&mut rx);
    let connecting_count = states.iter().filter(|s| **s == StartupState::Connecting).count();
    assert!(
        connecting_count >= 3,
        "expected ≥3 Connecting events, got {}", connecting_count
    );
}

#[tokio::test]
async fn tags_empty_models_array() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": []})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(
        states.iter().any(|s| matches!(s, StartupState::Failed(msg) if msg.contains("not found"))),
        "expected Failed with 'not found': {:?}", states
    );
}

#[tokio::test]
async fn tags_null_models_field() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"models": null})),
        )
        .mount(&server)
        .await;

    let port = server.address().port();
    let config = Config::new("127.0.0.1", port, "llama3");
    let (tx, mut rx) = mpsc::unbounded_channel();

    run_startup_checks(config, tx, fast_timings()).await;

    let states = collect_states(&mut rx);
    assert!(
        states.iter().any(|s| matches!(s, StartupState::Failed(msg) if msg.contains("not found"))),
        "expected Failed with 'not found': {:?}", states
    );
}
