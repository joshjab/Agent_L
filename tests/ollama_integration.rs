use agent_l::app::AppEvent;
use agent_l::ollama::fetch_ollama_stream;

use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Collect all events from the channel.
fn collect_events(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> Vec<AppEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn single_chunk_produces_token_and_done() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"message": {"content": "Hello"}})),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(e, AppEvent::Token(t) if t == "Hello")),
        "expected Token(\"Hello\")"
    );
    assert!(
        events.iter().any(|e| matches!(e, AppEvent::StreamDone)),
        "expected StreamDone"
    );
}

#[tokio::test]
async fn http_404_sends_error_token_and_done() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(e, AppEvent::Token(t) if t.contains("HTTP Error:"))),
        "expected Token with 'HTTP Error:'"
    );
    assert!(events.iter().any(|e| matches!(e, AppEvent::StreamDone)));
}

#[tokio::test]
async fn http_500_sends_error_token_and_done() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(e, AppEvent::Token(t) if t.contains("HTTP Error:"))),
        "expected Token with 'HTTP Error:'"
    );
    assert!(events.iter().any(|e| matches!(e, AppEvent::StreamDone)));
}

#[tokio::test]
async fn invalid_json_body_sends_parse_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(e, AppEvent::Token(t) if t.contains("[Parse Error on:"))),
        "expected Token with '[Parse Error on:'"
    );
    assert!(events.iter().any(|e| matches!(e, AppEvent::StreamDone)));
}

#[tokio::test]
async fn missing_content_field_no_token() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"message": {"role": "assistant"}})),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(
        !events.iter().any(|e| matches!(e, AppEvent::Token(_))),
        "expected no Token events"
    );
    assert!(events.iter().any(|e| matches!(e, AppEvent::StreamDone)));
}

#[tokio::test]
async fn empty_content_field_does_not_send_token() {
    // Empty content in the final done chunk should NOT produce a Token event —
    // empty tokens are noise and add nothing to the UI.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"message": {"content": ""}, "done": false})),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(
        !events.iter().any(|e| matches!(e, AppEvent::Token(t) if t.is_empty())),
        "empty-content chunks must not produce Token(\"\") events"
    );
    assert!(events.iter().any(|e| matches!(e, AppEvent::StreamDone)));
}

#[tokio::test]
async fn done_chunk_emits_token_stats() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                "{\"message\":{\"content\":\"hi\"},\"done\":false}\n\
                 {\"message\":{\"content\":\"\"},\"done\":true,\"prompt_eval_count\":42,\"eval_count\":7}\n"
            ),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "m", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    let stats = events.iter().find_map(|e| {
        if let AppEvent::TokenStats { prompt, generated } = e {
            Some((*prompt, *generated))
        } else {
            None
        }
    });
    assert_eq!(stats, Some((42, 7)), "expected TokenStats(42, 7) from done chunk");
}

#[tokio::test]
async fn request_payload_correct() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"message": {"content": "ok"}})),
        )
        .mount(&server)
        .await;

    let messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
    let (tx, _rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    fetch_ollama_stream(&url, "llama3", messages, tx).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "llama3");
    assert_eq!(body["stream"], true);
    assert!(body["messages"].is_array());
}

#[tokio::test]
async fn empty_messages_vec_ok() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"message": {"content": "ok"}})),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let url = format!("{}/api/chat", server.uri());
    // Should not panic with empty messages
    fetch_ollama_stream(&url, "llama3", vec![], tx).await.unwrap();

    let events = collect_events(&mut rx);
    assert!(events.iter().any(|e| matches!(e, AppEvent::StreamDone)));
}

#[tokio::test]
async fn dropped_receiver_does_not_panic() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"message": {"content": "hello"}})),
        )
        .mount(&server)
        .await;

    let (tx, rx) = mpsc::unbounded_channel::<AppEvent>();
    drop(rx); // Drop receiver before the call

    let url = format!("{}/api/chat", server.uri());
    let result = fetch_ollama_stream(&url, "llama3", vec![], tx).await;
    assert!(result.is_ok(), "should return Ok even with dropped receiver");
}

#[tokio::test]
async fn connection_refused_returns_err() {
    // Bind to get a free port then drop so nothing listens there
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let (tx, _rx) = mpsc::unbounded_channel();
    let url = format!("http://127.0.0.1:{}/api/chat", port);
    let result = fetch_ollama_stream(&url, "llama3", vec![], tx).await;
    assert!(result.is_err(), "expected Err for connection refused");
}
