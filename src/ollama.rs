use futures_util::StreamExt;
use crate::app::AppEvent;

/// Send a non-streaming POST request to `url` with the given JSON body and
/// return the raw response body as a string.
///
/// Connection-level errors (e.g. refused, timeout) are returned as `Err`.
/// HTTP-level errors (4xx, 5xx) are returned as `Ok(body_text)` so the
/// caller's parse step can handle them and trigger retries if appropriate.
pub async fn post_json(
    url: &str,
    body: serde_json::Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let text = client
        .post(url)
        .json(&body)
        .send()
        .await?
        .text()
        .await?;
    Ok(text)
}

pub async fn fetch_ollama_stream(
    url: &str,
    model: &str,
    messages: Vec<serde_json::Value>,
    tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let res = client
        .post(url)
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true
        }))
        .send()
        .await?;

    // Check if the server returned 404 or 500
    if !res.status().is_success() {
        let _ = tx.send(AppEvent::Token(format!("HTTP Error: {}", res.status())));
        let _ = tx.send(AppEvent::StreamDone);
        return Ok(());
    }

    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        match item {
            Ok(bytes) => {
                // Try to parse. If it fails, report the raw string for debugging.
                match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(body) => {
                        if let Some(token) = body["message"]["content"].as_str() {
                            let _ = tx.send(AppEvent::Token(token.to_string()));
                        }
                    },
                    Err(_) => {
                        // Sometimes Ollama sends multiple JSON objects in one chunk
                        // Let's try to convert the raw bytes to a string to see them
                        let raw = String::from_utf8_lossy(&bytes);
                        let _ = tx.send(AppEvent::Token(format!("\n[Parse Error on: {}]\n", raw)));
                    }
                }
            },
            Err(e) => {
                let _ = tx.send(AppEvent::Token(format!("\n[Stream Error: {}]\n", e)));
            }
        }
    }

    let _ = tx.send(AppEvent::StreamDone);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn post_json_returns_body_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"message":{"content":"hi"}}"#),
            )
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        let result = post_json(&url, serde_json::json!({"model": "test"})).await.unwrap();
        assert_eq!(result, r#"{"message":{"content":"hi"}}"#);
    }

    #[tokio::test]
    async fn post_json_returns_body_on_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        // HTTP errors don't raise Err — body is returned for the caller to handle
        let result = post_json(&url, serde_json::json!({})).await.unwrap();
        assert_eq!(result, "internal error");
    }

    #[tokio::test]
    async fn post_json_errors_on_connection_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = format!("http://127.0.0.1:{}/api/chat", port);
        let result = post_json(&url, serde_json::json!({})).await;
        assert!(result.is_err(), "connection refused should return Err");
    }
}
