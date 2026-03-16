use futures_util::StreamExt;
use crate::app::AppEvent;

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
