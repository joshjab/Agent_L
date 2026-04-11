use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::app::{AppEvent, StartupState};
use crate::config::Config;

pub struct StartupTimings {
    pub max_connect_retries: u32,
    pub connect_retry_delay: Duration,
    pub load_poll_interval: Duration,
    pub load_timeout: Duration,
}

impl Default for StartupTimings {
    fn default() -> Self {
        Self {
            max_connect_retries: 10,
            connect_retry_delay: Duration::from_secs(3),
            load_poll_interval: Duration::from_secs(1),
            load_timeout: Duration::from_secs(60),
        }
    }
}

fn model_matches(entry_name: &str, configured: &str) -> bool {
    entry_name == configured || entry_name.starts_with(&format!("{}:", configured))
}

pub async fn run_startup_checks(
    config: Config,
    tx: UnboundedSender<AppEvent>,
    timings: StartupTimings,
) {
    // Step 1: Connectivity — GET /api/tags with retry
    let tags_url = format!("{}/api/tags", config.base_url);
    let ps_url = format!("{}/api/ps", config.base_url);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut response_body: Option<serde_json::Value> = None;

    for attempt in 0..timings.max_connect_retries {
        let _ = tx.send(AppEvent::StartupUpdate(StartupState::Connecting));

        match client.get(&tags_url).send().await {
            Ok(res) if res.status().is_success() => {
                if let Ok(body) = res.json::<serde_json::Value>().await {
                    response_body = Some(body);
                    break;
                }
            }
            _ => {}
        }

        if attempt + 1 < timings.max_connect_retries {
            tokio::time::sleep(timings.connect_retry_delay).await;
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
    let model_found = models.is_some_and(|arr| {
        arr.iter().any(|m| {
            m["name"]
                .as_str()
                .is_some_and(|name| model_matches(name, &config.model_name))
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
                    let loaded = ps_body["models"].as_array().is_some_and(|arr| {
                        arr.iter().any(|m| {
                            m["name"]
                                .as_str()
                                .is_some_and(|name| model_matches(name, &config.model_name))
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

        if start.elapsed() >= timings.load_timeout {
            // Timeout — let the user try anyway
            let _ = tx.send(AppEvent::StartupUpdate(StartupState::Ready));
            return;
        }

        tokio::time::sleep(timings.load_poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(model_matches("llama3", "llama3"));
    }

    #[test]
    fn test_tag_suffix_match() {
        assert!(model_matches("llama3:latest", "llama3"));
    }

    #[test]
    fn test_multiple_tag_variants() {
        assert!(model_matches("llama3:7b-instruct", "llama3"));
    }

    #[test]
    fn test_no_match_different_name() {
        assert!(!model_matches("codellama", "llama3"));
    }

    #[test]
    fn test_no_match_prefix_without_colon() {
        assert!(!model_matches("llama3extra", "llama3"));
    }

    #[test]
    fn test_both_empty() {
        assert!(model_matches("", ""));
    }

    #[test]
    fn test_nonempty_vs_empty() {
        assert!(!model_matches("llama3", ""));
    }

    #[test]
    fn test_exact_with_colon() {
        assert!(model_matches("llama3:latest", "llama3:latest"));
    }
}
