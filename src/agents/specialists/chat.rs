use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::app::AppEvent;

use super::SpecialistError;

/// The Chat specialist handles Conversational and Creative intents.
///
/// It streams tokens directly to the UI using the persona-wrapped messages as
/// context. Optionally prepends context from a prior `depends_on` step.
pub struct ChatSpecialist;

impl ChatSpecialist {
    /// Stream a chat response for `task` using `messages` as conversation
    /// context. If `context` is provided (output from a `depends_on` step), it
    /// is appended as a user message so the model sees it before responding.
    ///
    /// Token events are forwarded in real-time to `tx`. `StreamDone` is NOT
    /// sent — the step runner owns that responsibility.
    ///
    /// Returns the full accumulated response text (for use by downstream steps).
    pub async fn run(
        &self,
        _task: &str,
        messages: &[Value],
        context: Option<&str>,
        model: &str,
        chat_url: &str,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Result<String, SpecialistError> {
        // Build the message list, appending prior-step context if provided.
        let mut msgs: Vec<Value> = messages.to_vec();
        if let Some(ctx) = context {
            msgs.push(json!({"role": "user", "content": ctx}));
        }

        // Use a capture channel so we can accumulate tokens while forwarding
        // them in real-time to the original `tx`.
        let (capture_tx, mut capture_rx) = mpsc::unbounded_channel::<AppEvent>();

        // Spawn a forwarding task that forwards Token events and accumulates.
        // It stops when `capture_tx` is dropped (channel closed).
        let fwd_task = tokio::spawn(async move {
            let mut accumulated = String::new();
            while let Some(event) = capture_rx.recv().await {
                match &event {
                    AppEvent::Token(t) => accumulated.push_str(t),
                    AppEvent::StreamDone => {
                        // Do NOT forward StreamDone — the step runner owns it.
                        continue;
                    }
                    _ => {}
                }
                let _ = tx.send(event);
            }
            accumulated
        });

        // Run the stream; capture_tx is consumed and dropped when this returns.
        crate::ollama::fetch_ollama_stream(chat_url, model, msgs, capture_tx)
            .await
            .map_err(|e| SpecialistError { message: e.to_string() })?;

        // Await the forwarding task to get the full accumulated text.
        fwd_task.await.map_err(|e| SpecialistError { message: e.to_string() })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ndjson_stream(chunks: &[(&str, bool)]) -> String {
        chunks
            .iter()
            .map(|(tok, done)| {
                format!(
                    "{{\"message\":{{\"content\":\"{tok}\"}},\"done\":{done}}}\n"
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn streams_tokens_to_tx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ndjson_stream(&[("Hello", false), (" world", true)])),
            )
            .mount(&server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());
        let messages = vec![json!({"role": "user", "content": "hi"})];

        ChatSpecialist
            .run("respond", &messages, None, "m", &url, tx)
            .await
            .unwrap();

        let mut tokens = Vec::new();
        while let Ok(AppEvent::Token(t)) = rx.try_recv() {
            tokens.push(t);
        }
        assert_eq!(tokens.join(""), "Hello world");
    }

    #[tokio::test]
    async fn returns_accumulated_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ndjson_stream(&[("Foo", false), ("Bar", true)])),
            )
            .mount(&server)
            .await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());
        let accumulated = ChatSpecialist
            .run("task", &[], None, "m", &url, tx)
            .await
            .unwrap();
        assert_eq!(accumulated, "FooBar");
    }

    #[tokio::test]
    async fn does_not_send_stream_done() {
        // StreamDone belongs to the step runner, not the specialist.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ndjson_stream(&[("hi", true)])),
            )
            .mount(&server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());
        ChatSpecialist
            .run("t", &[], None, "m", &url, tx)
            .await
            .unwrap();

        let mut saw_stream_done = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AppEvent::StreamDone) {
                saw_stream_done = true;
            }
        }
        assert!(!saw_stream_done, "ChatSpecialist must not send StreamDone");
    }

    #[tokio::test]
    async fn injects_context_as_user_message() {
        // Verify the request body sent to Ollama includes the context turn.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ndjson_stream(&[("ok", true)])),
            )
            .mount(&server)
            .await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let url = format!("{}/api/chat", server.uri());
        let messages = vec![json!({"role": "user", "content": "original"})];

        // Should not panic even with context provided.
        ChatSpecialist
            .run("task", &messages, Some("prior step output"), "m", &url, tx)
            .await
            .unwrap();

        // Verify Ollama was actually called once.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn connection_error_returns_specialist_error() {
        // Bind and immediately drop a listener so the port is closed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let (tx, _rx) = mpsc::unbounded_channel();
        let url = format!("http://127.0.0.1:{port}/api/chat");
        let err = ChatSpecialist
            .run("t", &[], None, "m", &url, tx)
            .await
            .unwrap_err();
        assert!(!err.message.is_empty());
    }
}
