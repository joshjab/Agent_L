pub mod compression;
pub mod orchestrator;
pub mod persona;
pub mod schema;
pub mod specialists;

use schema::ParseError;
use serde_json::Value;
use std::future::Future;

/// Returned when all retry attempts are exhausted or an HTTP error occurs.
#[derive(Debug)]
pub struct AgentError {
    /// Number of attempts made before giving up.
    pub attempts: u8,
    /// Description of the last failure.
    pub last_error: String,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent failed after {} attempt(s): {}", self.attempts, self.last_error)
    }
}

impl std::error::Error for AgentError {}

/// Core trait for all agent roles (orchestrator, specialists, etc.).
///
/// An agent knows how to build its own Ollama request body and how to
/// validate the response. The retry loop lives in [`call_with_retry`] so it
/// can be tested with a mock HTTP function instead of a real Ollama instance.
pub trait Agent {
    type Output;

    /// Build the Ollama request body (model, messages, `format` JSON schema, `stream: false`).
    ///
    /// `error_feedback` is `Some(err)` on a retry. Implementations should
    /// append the error text to the messages so the model can self-correct.
    fn prompt(&self, context: &[Value], error_feedback: Option<&ParseError>) -> Value;

    /// Parse and validate the raw JSON string returned by Ollama.
    ///
    /// Return `Err(ParseError)` if any required field is missing or the wrong type.
    fn parse(&self, response: &str) -> Result<Self::Output, ParseError>;
}

/// Call `agent` with retry. On parse failure, re-calls `agent.prompt()` with
/// the error injected so the model can self-correct. Returns `Err(AgentError)`
/// once `max_attempts` are exhausted.
///
/// `post_fn` sends the Ollama request body and returns the raw response string.
/// Pass a mock closure in tests; pass the real Ollama HTTP call in production.
pub async fn call_with_retry<A, F, Fut>(
    agent: &A,
    context: &[Value],
    post_fn: F,
    max_attempts: u8,
) -> Result<A::Output, AgentError>
where
    A: Agent,
    F: Fn(Value) -> Fut,
    Fut: Future<Output = Result<String, Box<dyn std::error::Error>>>,
{
    assert!(max_attempts > 0, "max_attempts must be at least 1");

    let mut last_parse_error: Option<ParseError> = None;

    for attempt in 0..max_attempts {
        let request = agent.prompt(context, last_parse_error.as_ref());

        let raw = match post_fn(request).await {
            Ok(r) => r,
            Err(e) => {
                return Err(AgentError {
                    attempts: attempt + 1,
                    last_error: e.to_string(),
                });
            }
        };

        match agent.parse(&raw) {
            Ok(output) => return Ok(output),
            Err(err) => last_parse_error = Some(err),
        }
    }

    Err(AgentError {
        attempts: max_attempts,
        last_error: last_parse_error
            .map(|e| e.message)
            .unwrap_or_else(|| "unknown parse error".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    // Parses `{"result": "<string>"}` — succeeds on well-formed input.
    struct OkAgent;

    impl Agent for OkAgent {
        type Output = String;

        fn prompt(&self, _ctx: &[Value], _err: Option<&ParseError>) -> Value {
            json!({ "model": "test", "messages": [], "stream": false })
        }

        fn parse(&self, response: &str) -> Result<String, ParseError> {
            let v: Value = serde_json::from_str(response).map_err(|e| ParseError {
                message: e.to_string(),
            })?;
            v.get("result")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| ParseError {
                    message: "missing 'result' field".into(),
                })
        }
    }

    // Always returns a parse error regardless of input.
    struct AlwaysFailAgent;

    impl Agent for AlwaysFailAgent {
        type Output = String;

        fn prompt(&self, _ctx: &[Value], _err: Option<&ParseError>) -> Value {
            json!({})
        }

        fn parse(&self, _response: &str) -> Result<String, ParseError> {
            Err(ParseError { message: "always fails".into() })
        }
    }

    // Records whether each prompt() call received error feedback.
    struct FeedbackTrackingAgent {
        calls: Arc<Mutex<Vec<bool>>>, // true = had error feedback on that call
    }

    impl Agent for FeedbackTrackingAgent {
        type Output = String;

        fn prompt(&self, _ctx: &[Value], error_feedback: Option<&ParseError>) -> Value {
            self.calls.lock().unwrap().push(error_feedback.is_some());
            json!({})
        }

        fn parse(&self, _: &str) -> Result<String, ParseError> {
            Err(ParseError { message: "fail".into() })
        }
    }

    #[tokio::test]
    async fn succeeds_on_first_attempt() {
        let result = call_with_retry(
            &OkAgent,
            &[],
            |_req| async { Ok::<String, Box<dyn std::error::Error>>(r#"{"result":"hello"}"#.into()) },
            3,
        )
        .await;
        assert_eq!(result.unwrap(), "hello");
    }

    #[tokio::test]
    async fn returns_error_after_all_attempts_exhausted() {
        let err = call_with_retry(
            &AlwaysFailAgent,
            &[],
            |_req| async { Ok::<String, Box<dyn std::error::Error>>("{}".into()) },
            3,
        )
        .await
        .unwrap_err();

        assert_eq!(err.attempts, 3);
        assert!(err.last_error.contains("always fails"));
    }

    #[tokio::test]
    async fn injects_error_feedback_on_retry() {
        let calls: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let agent = FeedbackTrackingAgent { calls: calls.clone() };

        let _ = call_with_retry(
            &agent,
            &[],
            |_req| async { Ok::<String, Box<dyn std::error::Error>>("{}".into()) },
            3,
        )
        .await;

        let log = calls.lock().unwrap().clone();
        assert_eq!(log.len(), 3, "prompt() should be called 3 times");
        assert!(!log[0], "first call: no error feedback yet");
        assert!(log[1], "second call: error feedback from attempt 1");
        assert!(log[2], "third call: error feedback from attempt 2");
    }

    #[tokio::test]
    async fn http_error_stops_immediately_without_retrying() {
        let err = call_with_retry(
            &OkAgent,
            &[],
            |_req| async {
                Err::<String, Box<dyn std::error::Error>>("connection refused".into())
            },
            3,
        )
        .await
        .unwrap_err();

        assert_eq!(err.attempts, 1, "HTTP errors should not be retried");
    }
}
