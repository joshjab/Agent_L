use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::app::{AppEvent, StartupState};
use crate::config::Config;

const MAX_CONNECT_RETRIES: u32 = 10;
const CONNECT_RETRY_DELAY: Duration = Duration::from_secs(3);
const LOAD_POLL_INTERVAL: Duration = Duration::from_secs(1);
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);

fn model_matches(entry_name: &str, configured: &str) -> bool {
    entry_name == configured || entry_name.starts_with(&format!("{}:", configured))
}

pub async fn run_startup_checks(config: Config, tx: UnboundedSender<AppEvent>) {
    // Step 1: Connectivity — GET /api/tags with retry
    let tags_url = format!("{}/api/tags", config.base_url);
    let ps_url = format!("{}/api/ps", config.base_url);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut response_body: Option<serde_json::Value> = None;

    for attempt in 0..MAX_CONNECT_RETRIES {
        let _ = tx.send(AppEvent::StartupUpdate(StartupState::Connecting));

        match client.get(&tags_url).send().await {
            Ok(res) if res.status().is_success() => {
                match res.json::<serde_json::Value>().await {
                    Ok(body) => {
                        response_body = Some(body);
                        break;
                    }
                    Err(_) => {}
                }
            }
            _ => {}
        }

        if attempt + 1 < MAX_CONNECT_RETRIES {
            tokio::time::sleep(CONNECT_RETRY_DELAY).await;
        }
    }

    let body = match response_body {
        Some(b) => b,
        None => {
            let msg = format!(
                "Cannot reach Ollama after 30s. Is it running at {}?",
                config.base_url
            );
            let _ = tx.send(AppEvent::StartupUpdate(StartupState::Failed(msg)));
            return;
        }
    };

    // Step 2: Model existence — parse /api/tags body
    let _ = tx.send(AppEvent::StartupUpdate(StartupState::CheckingModel));

    let models = body["models"].as_array();
    let model_found = models.map_or(false, |arr| {
        arr.iter().any(|m| {
            m["name"].as_str().map_or(false, |name| model_matches(name, &config.model_name))
        })
    });

    if !model_found {
        let msg = format!(
            "Model '{}' not found locally. Run: ollama pull {}",
            config.model_name, config.model_name
        );
        let _ = tx.send(AppEvent::StartupUpdate(StartupState::Failed(msg)));
        return;
    }

    // Step 3: Load polling — GET /api/ps until model appears or timeout
    let _ = tx.send(AppEvent::StartupUpdate(StartupState::LoadingModel));

    let start = Instant::now();
    loop {
        match client.get(&ps_url).send().await {
            Ok(res) if res.status().is_success() => {
                if let Ok(ps_body) = res.json::<serde_json::Value>().await {
                    let loaded = ps_body["models"].as_array().map_or(false, |arr| {
                        arr.iter().any(|m| {
                            m["name"].as_str().map_or(false, |name| model_matches(name, &config.model_name))
                        })
                    });
                    if loaded {
                        let _ = tx.send(AppEvent::StartupUpdate(StartupState::Ready));
                        return;
                    }
                }
            }
            _ => {}
        }

        if start.elapsed() >= LOAD_TIMEOUT {
            // Timeout — let the user try anyway
            let _ = tx.send(AppEvent::StartupUpdate(StartupState::Ready));
            return;
        }

        tokio::time::sleep(LOAD_POLL_INTERVAL).await;
    }
}
