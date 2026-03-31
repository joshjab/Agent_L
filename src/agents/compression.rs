use serde_json::{json, Value};
use std::future::Future;

use crate::agents::AgentError;

pub const DEFAULT_TOKEN_THRESHOLD: usize = 2000;
pub const DEFAULT_TURNS_TO_COMPRESS: usize = 10;

/// Prefix that marks a synthetic summary message in the conversation.
pub const SUMMARY_TAG: &str = "[Context Summary]";

/// Rough token estimate: total character count of all message `content` fields
/// divided by 4 (1 token ≈ 4 characters).
pub fn estimate_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .filter_map(|m| m.get("content")?.as_str())
        .map(|s| s.len())
        .sum::<usize>()
        / 4
}

/// Compresses old conversation history by summarising the oldest turns into a
/// single context-summary message when the total token estimate exceeds a
/// threshold.
pub struct Compressor {
    pub threshold: usize,
    pub turns_to_compress: usize,
}

impl Compressor {
    /// Create a compressor with production defaults.
    pub fn new() -> Self {
        Self {
            threshold: DEFAULT_TOKEN_THRESHOLD,
            turns_to_compress: DEFAULT_TURNS_TO_COMPRESS,
        }
    }

    /// Create a compressor with custom parameters. Used in unit tests only.
    #[cfg(test)]
    pub fn with_params(threshold: usize, turns_to_compress: usize) -> Self {
        Self { threshold, turns_to_compress }
    }

    /// Build an Ollama request body that asks the model to summarise `turns`.
    pub fn build_summary_prompt(&self, turns: &[Value], model: &str) -> Value {
        // Format the turns into readable text for the summarisation prompt.
        let mut conversation = String::new();
        for turn in turns {
            let role = turn["role"].as_str().unwrap_or("unknown");
            let content = turn["content"].as_str().unwrap_or("");
            conversation.push_str(&format!("{}: {}\n", role, content));
        }

        json!({
            "model": model,
            "stream": false,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a summarisation assistant. Summarise the following conversation turns into a single concise paragraph. Preserve all key facts, decisions, and context the user and assistant established."
                },
                {
                    "role": "user",
                    "content": format!("Conversation to summarise:\n\n{}", conversation.trim_end())
                }
            ]
        })
    }

    /// If `history` exceeds the token threshold, summarise the oldest
    /// `turns_to_compress` messages into a single `[Context Summary] …` system
    /// message and replace them. Returns `history` unchanged when below the
    /// threshold or when `history` is empty.
    ///
    /// `actual_tokens` — when `Some`, uses the real `prompt_eval_count` from
    /// the most recent Ollama call instead of the char/4 estimate. Pass `None`
    /// before the first call has completed.
    ///
    /// `post` sends the Ollama request body and returns the raw response string.
    /// Pass a mock closure in tests; pass the real HTTP call in production.
    pub async fn maybe_compress<F, Fut>(
        &self,
        history: Vec<Value>,
        model: &str,
        actual_tokens: Option<u32>,
        post: F,
    ) -> Result<Vec<Value>, AgentError>
    where
        F: Fn(Value) -> Fut,
        Fut: Future<Output = Result<String, Box<dyn std::error::Error>>>,
    {
        let token_count = actual_tokens
            .map(|t| t as usize)
            .unwrap_or_else(|| estimate_tokens(&history));

        if history.is_empty() || token_count <= self.threshold {
            return Ok(history);
        }

        let n = self.turns_to_compress.min(history.len());
        let to_compress = &history[..n];
        let remaining = history[n..].to_vec();

        let request = self.build_summary_prompt(to_compress, model);
        let raw = post(request).await.map_err(|e| AgentError {
            attempts: 1,
            last_error: e.to_string(),
        })?;

        let summary_text = extract_content(&raw).map_err(|e| AgentError {
            attempts: 1,
            last_error: e,
        })?;

        let summary_message = json!({
            "role": "system",
            "content": format!("{} {}", SUMMARY_TAG, summary_text)
        });

        let mut result = Vec::with_capacity(1 + remaining.len());
        result.push(summary_message);
        result.extend(remaining);
        Ok(result)
    }
}

/// Parse the summary text out of an Ollama non-streaming response envelope.
///
/// Expected format: `{"message": {"content": "..."}}`
fn extract_content(raw: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| format!("invalid JSON from Ollama: {e}"))?;

    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "missing message.content in Ollama response".to_string())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_msg(content: &str) -> Value {
        json!({"role": "user", "content": content})
    }

    fn assistant_msg(content: &str) -> Value {
        json!({"role": "assistant", "content": content})
    }

    /// Mock Ollama response containing a summary that mentions `keyword`.
    fn mock_ollama_response(summary: &str) -> String {
        json!({"message": {"content": summary}}).to_string()
    }

    // ── estimate_tokens ──────────────────────────────────────────────────────

    #[test]
    fn estimate_tokens_empty_returns_zero() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_counts_chars_over_four() {
        // "hello" = 5 chars → 5/4 = 1 token (integer division)
        let msgs = vec![user_msg("hello")];
        assert_eq!(estimate_tokens(&msgs), 1);
    }

    #[test]
    fn estimate_tokens_sums_all_content_fields() {
        // "abcd" = 4 chars → 1 token; "efgh" = 4 chars → 1 token; total = 2
        let msgs = vec![user_msg("abcd"), assistant_msg("efgh")];
        assert_eq!(estimate_tokens(&msgs), 2);
    }

    #[test]
    fn estimate_tokens_ignores_non_content_fields() {
        // Only the "content" field should be counted, not "role"
        let msgs = vec![json!({"role": "user", "content": "ab"})];
        // "ab" = 2 chars → 2/4 = 0 tokens
        assert_eq!(estimate_tokens(&msgs), 0);
    }

    // ── Compressor::new / with_params ────────────────────────────────────────

    #[test]
    fn new_uses_default_params() {
        let c = Compressor::new();
        assert_eq!(c.threshold, DEFAULT_TOKEN_THRESHOLD);
        assert_eq!(c.turns_to_compress, DEFAULT_TURNS_TO_COMPRESS);
    }

    #[test]
    fn with_params_stores_given_values() {
        let c = Compressor::with_params(500, 4);
        assert_eq!(c.threshold, 500);
        assert_eq!(c.turns_to_compress, 4);
    }

    // ── build_summary_prompt ─────────────────────────────────────────────────

    #[test]
    fn build_summary_prompt_contains_model_and_no_stream() {
        let c = Compressor::with_params(100, 4);
        let turns = vec![user_msg("hi")];
        let req = c.build_summary_prompt(&turns, "mymodel");
        assert_eq!(req["model"], "mymodel");
        assert_eq!(req["stream"], false);
    }

    #[test]
    fn build_summary_prompt_messages_is_nonempty() {
        let c = Compressor::with_params(100, 4);
        let turns = vec![user_msg("hello"), assistant_msg("world")];
        let req = c.build_summary_prompt(&turns, "m");
        assert!(req["messages"].as_array().unwrap().len() >= 1);
    }

    // ── maybe_compress ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn below_threshold_returns_history_unchanged() {
        let c = Compressor::with_params(10_000, 4); // very high threshold
        let history = vec![user_msg("hi"), assistant_msg("hey")];
        let result = c
            .maybe_compress(history.clone(), "m", None, |_req| async {
                panic!("post should not be called below threshold")
            })
            .await
            .unwrap();
        assert_eq!(result, history);
    }

    #[tokio::test]
    async fn empty_history_returns_empty() {
        let c = Compressor::with_params(0, 4); // threshold of 0 → always triggers
        let result = c
            .maybe_compress(vec![], "m", None, |_req| async {
                panic!("post should not be called on empty history")
            })
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn above_threshold_calls_post_and_compresses() {
        // Very low threshold + small history to guarantee trigger.
        // Compressor will compress the first `turns_to_compress` messages.
        let turns_to_compress = 2;
        let c = Compressor::with_params(0, turns_to_compress); // threshold 0 → always compresses

        let history = vec![
            user_msg("question about foo"),
            assistant_msg("answer about bar"),
            user_msg("follow-up"),
        ];
        let summary_text = "Summary: user asked about foo, assistant answered about bar";

        let result = c
            .maybe_compress(history.clone(), "m", None, |_req| async move {
                Ok::<String, Box<dyn std::error::Error>>(mock_ollama_response(summary_text))
            })
            .await
            .unwrap();

        // Output should have fewer messages than input
        assert!(
            result.len() < history.len(),
            "expected fewer messages after compression, got {} (was {})",
            result.len(),
            history.len()
        );
    }

    #[tokio::test]
    async fn compressed_output_starts_with_summary_tag() {
        let c = Compressor::with_params(0, 2);
        let history = vec![
            user_msg("tell me about rust"),
            assistant_msg("rust is a systems language"),
            user_msg("nice"),
        ];
        let summary_text = "Summary: rust discussion";

        let result = c
            .maybe_compress(history, "m", None, |_req| async move {
                Ok::<String, Box<dyn std::error::Error>>(mock_ollama_response(summary_text))
            })
            .await
            .unwrap();

        let first_content = result[0]["content"]
            .as_str()
            .expect("first message should have a content string");
        assert!(
            first_content.starts_with(SUMMARY_TAG),
            "first message content should start with '{SUMMARY_TAG}', got: {first_content:?}"
        );
    }

    #[tokio::test]
    async fn summary_content_appears_in_output() {
        let c = Compressor::with_params(0, 2);
        let history = vec![
            user_msg("tell me about tokio"),
            assistant_msg("tokio is an async runtime"),
            user_msg("thanks"),
        ];
        let keyword = "tokio-runtime-summary-keyword";
        let summary_text = format!("Summary mentions {keyword}");

        let result = c
            .maybe_compress(history, "m", None, move |_req| {
                let s = summary_text.clone();
                async move { Ok::<String, Box<dyn std::error::Error>>(mock_ollama_response(&s)) }
            })
            .await
            .unwrap();

        let first_content = result[0]["content"].as_str().unwrap();
        assert!(
            first_content.contains(keyword),
            "expected keyword '{keyword}' in summary, got: {first_content:?}"
        );
    }

    #[tokio::test]
    async fn remaining_turns_preserved_after_compression() {
        let c = Compressor::with_params(0, 2); // compress first 2
        let history = vec![
            user_msg("old message 1"),
            assistant_msg("old reply 1"),
            user_msg("recent message"),
        ];

        let result = c
            .maybe_compress(history, "m", None, |_req| async {
                Ok::<String, Box<dyn std::error::Error>>(mock_ollama_response("summary"))
            })
            .await
            .unwrap();

        // The most recent message should still be present
        let last = &result[result.len() - 1];
        assert_eq!(last["content"], "recent message");
    }

    #[tokio::test]
    async fn post_error_returns_agent_error() {
        let c = Compressor::with_params(0, 2);
        // Messages must have enough content to exceed threshold=0 (>4 chars total).
        let history = vec![user_msg("hello there"), assistant_msg("greetings"), user_msg("bye")];

        let err = c
            .maybe_compress(history, "m", None, |_req| async {
                Err::<String, Box<dyn std::error::Error>>("connection refused".into())
            })
            .await
            .unwrap_err();

        assert!(err.last_error.contains("connection refused"));
    }

    #[tokio::test]
    async fn actual_tokens_overrides_estimate_for_threshold() {
        // history has tiny content (would be ~0 estimated tokens),
        // but actual_tokens = Some(5000) should trigger compression even with
        // a threshold of 2000.
        let c = Compressor::with_params(2000, 2);
        let history = vec![user_msg("hi"), assistant_msg("hey"), user_msg("ok")];

        let result = c
            .maybe_compress(history.clone(), "m", Some(5000), |_req| async {
                Ok::<String, Box<dyn std::error::Error>>(mock_ollama_response("summary"))
            })
            .await
            .unwrap();

        // Compression should have run (fewer messages than input)
        assert!(result.len() < history.len(), "actual_tokens should have triggered compression");
    }

    #[tokio::test]
    async fn actual_tokens_below_threshold_skips_compression() {
        // history has large content, but actual_tokens says we're under threshold.
        let c = Compressor::with_params(2000, 2);
        // Build enough content to exceed estimate threshold but pass actual < 2000
        let big_content = "a".repeat(10_000); // 10_000 chars → 2500 estimated tokens
        let history = vec![user_msg(&big_content), assistant_msg("reply"), user_msg("ok")];

        let result = c
            .maybe_compress(history.clone(), "m", Some(100), |_req| async {
                panic!("post should not be called when actual_tokens is below threshold")
            })
            .await
            .unwrap();

        assert_eq!(result, history);
    }
}
