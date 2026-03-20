use std::collections::HashMap;

use serde_json::{json, Value};

use super::Tool;

/// Maximum number of ReAct steps before the executor hard-stops with an error.
pub const MAX_STEPS: usize = 10;

/// A single parsed output from the model during a ReAct loop iteration.
#[derive(Debug, PartialEq)]
pub enum ReActStep {
    /// The model is reasoning. Content is the thought text.
    Thought(String),
    /// The model wants to call a tool.
    ToolCall { name: String, args: Value },
    /// The model has produced a final answer.
    FinalAnswer(String),
}

/// Error returned by [`execute_react_loop`].
#[derive(Debug)]
pub struct ExecutorError {
    pub message: String,
    /// Number of steps taken before failure.
    pub steps_taken: usize,
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "executor error after {} steps: {}", self.steps_taken, self.message)
    }
}

impl std::error::Error for ExecutorError {}

/// Parse a single ReAct-formatted line produced by the model.
///
/// Expected formats:
/// - `Thought: <text>`
/// - `ToolCall: <name> <json_args>`
/// - `FinalAnswer: <text>`
///
/// Returns `None` if the line doesn't match any known prefix.
pub fn parse_react_line(line: &str) -> Option<ReActStep> {
    if let Some(rest) = line.strip_prefix("Thought: ") {
        Some(ReActStep::Thought(rest.to_string()))
    } else if let Some(rest) = line.strip_prefix("FinalAnswer: ") {
        Some(ReActStep::FinalAnswer(rest.to_string()))
    } else if let Some(rest) = line.strip_prefix("ToolCall: ") {
        // Format: "<name> <json_args>"
        let (name, args_str) = rest.split_once(' ')?;
        let args: Value = serde_json::from_str(args_str).ok()?;
        Some(ReActStep::ToolCall { name: name.to_string(), args })
    } else {
        None
    }
}

/// Validate `args` against the tool's JSON Schema (checks required fields only).
///
/// Returns `Ok(())` if all `required` fields listed in the schema are present
/// in `args`, otherwise `Err` with a description of the missing field.
pub fn validate_args(schema: &Value, args: &Value) -> Result<(), String> {
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for field in required {
            if let Some(name) = field.as_str() {
                if args.get(name).is_none() {
                    return Err(format!("missing required field: '{name}'"));
                }
            }
        }
    }
    Ok(())
}

/// Run the ReAct loop for a specialist.
///
/// `prompt_fn` sends the current message list to the model and returns the
/// model's raw text response (not streaming). Pass a mock closure in tests;
/// pass the real Ollama HTTP call in production.
///
/// The loop:
/// 1. Calls `prompt_fn` with the current messages.
/// 2. Parses the response as a sequence of ReAct steps.
/// 3. For `ToolCall`: validates args, calls `execute()`, appends `Observation:`.
/// 4. For `FinalAnswer`: returns the answer text.
/// 5. Hard-stops after `max_steps` total steps with `ExecutorError`.
pub async fn execute_react_loop<F, Fut>(
    initial_messages: Vec<Value>,
    tools: &HashMap<&str, &dyn Tool>,
    prompt_fn: F,
    max_steps: usize,
) -> Result<String, ExecutorError>
where
    F: Fn(Vec<Value>) -> Fut,
    Fut: std::future::Future<Output = Result<String, Box<dyn std::error::Error>>>,
{
    let mut messages = initial_messages;

    for step in 0..max_steps {
        let response = prompt_fn(messages.clone())
            .await
            .map_err(|e| ExecutorError { message: e.to_string(), steps_taken: step + 1 })?;

        // Append the model's response as an assistant message.
        messages.push(json!({"role": "assistant", "content": response}));

        // Parse each line for ReAct actions.
        let mut final_answer: Option<String> = None;
        let mut observations: Vec<String> = Vec::new();

        for line in response.lines() {
            match parse_react_line(line.trim()) {
                Some(ReActStep::FinalAnswer(ans)) => {
                    final_answer = Some(ans);
                    break;
                }
                Some(ReActStep::ToolCall { name, args }) => {
                    let obs = match tools.get(name.as_str()) {
                        Some(tool) => match validate_args(&tool.schema(), &args) {
                            Ok(()) => match tool.execute(&args) {
                                Ok(result) => format!("Observation: {result}"),
                                Err(e) => format!("Observation: Error: {e}"),
                            },
                            Err(e) => format!("Observation: Validation Error: {e}"),
                        },
                        None => format!("Observation: Error: unknown tool '{name}'"),
                    };
                    observations.push(obs);
                }
                Some(ReActStep::Thought(_)) | None => {
                    // Thoughts are already captured in the assistant message.
                }
            }
        }

        if let Some(ans) = final_answer {
            return Ok(ans);
        }

        // Append all tool observations as user messages for the next iteration.
        for obs in observations {
            messages.push(json!({"role": "user", "content": obs}));
        }
    }

    Err(ExecutorError {
        message: format!("reached step limit ({max_steps}) without a FinalAnswer"),
        steps_taken: max_steps,
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_tools::{AlwaysFailTool, AlwaysOkTool};
    use serde_json::json;

    // ── parse_react_line ─────────────────────────────────────────────────────

    #[test]
    fn parse_thought_line() {
        let step = parse_react_line("Thought: I need to search for this").unwrap();
        assert_eq!(step, ReActStep::Thought("I need to search for this".into()));
    }

    #[test]
    fn parse_final_answer_line() {
        let step = parse_react_line("FinalAnswer: The capital is Paris").unwrap();
        assert_eq!(step, ReActStep::FinalAnswer("The capital is Paris".into()));
    }

    #[test]
    fn parse_tool_call_line() {
        let step =
            parse_react_line(r#"ToolCall: web_search {"query":"rust lang"}"#).unwrap();
        assert_eq!(
            step,
            ReActStep::ToolCall {
                name: "web_search".into(),
                args: json!({"query": "rust lang"}),
            }
        );
    }

    #[test]
    fn parse_unknown_line_returns_none() {
        assert!(parse_react_line("random text with no prefix").is_none());
        assert!(parse_react_line("").is_none());
    }

    // ── validate_args ────────────────────────────────────────────────────────

    #[test]
    fn validate_args_passes_when_required_fields_present() {
        let schema = json!({
            "type": "object",
            "required": ["query"],
            "properties": {"query": {"type": "string"}}
        });
        let args = json!({"query": "hello"});
        assert!(validate_args(&schema, &args).is_ok());
    }

    #[test]
    fn validate_args_fails_when_required_field_missing() {
        let schema = json!({
            "type": "object",
            "required": ["query"],
            "properties": {"query": {"type": "string"}}
        });
        let args = json!({});
        assert!(validate_args(&schema, &args).is_err());
    }

    #[test]
    fn validate_args_passes_with_no_required_fields() {
        let schema = json!({"type": "object", "properties": {}});
        let args = json!({});
        assert!(validate_args(&schema, &args).is_ok());
    }

    // ── execute_react_loop ───────────────────────────────────────────────────

    #[tokio::test]
    async fn loop_returns_final_answer_on_success() {
        let tools: HashMap<&str, &dyn Tool> = HashMap::new();

        let result = execute_react_loop(
            vec![json!({"role": "user", "content": "hello"})],
            &tools,
            |_msgs| async { Ok::<String, Box<dyn std::error::Error>>("FinalAnswer: done".into()) },
            MAX_STEPS,
        )
        .await
        .unwrap();

        assert_eq!(result, "done");
    }

    #[tokio::test]
    async fn loop_executes_tool_then_gets_final_answer() {
        let ok_tool = AlwaysOkTool { name: "ok_tool" };
        let mut tools: HashMap<&str, &dyn Tool> = HashMap::new();
        tools.insert("ok_tool", &ok_tool);

        let calls = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let calls_clone = calls.clone();

        let result = execute_react_loop(
            vec![json!({"role": "user", "content": "use ok_tool"})],
            &tools,
            move |_msgs| {
                let mut c = calls_clone.lock().unwrap();
                *c += 1;
                let call_num = *c;
                async move {
                    if call_num == 1 {
                        Ok::<String, Box<dyn std::error::Error>>(
                            r#"ToolCall: ok_tool {}"#.into(),
                        )
                    } else {
                        Ok("FinalAnswer: got mock result".into())
                    }
                }
            },
            MAX_STEPS,
        )
        .await
        .unwrap();

        assert_eq!(result, "got mock result");
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn loop_appends_observation_after_tool_call() {
        // The tool returns "tool output". After the tool call, the next prompt
        // call should receive a message containing "Observation: tool output".
        let ok_tool = AlwaysOkTool { name: "ok_tool" };
        let mut tools: HashMap<&str, &dyn Tool> = HashMap::new();
        tools.insert("ok_tool", &ok_tool);

        let last_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let lm_clone = last_messages.clone();

        let _ = execute_react_loop(
            vec![json!({"role": "user", "content": "go"})],
            &tools,
            move |msgs| {
                let mut lm = lm_clone.lock().unwrap();
                *lm = msgs.clone();
                let n = lm.len();
                async move {
                    if n == 1 {
                        Ok::<String, Box<dyn std::error::Error>>(
                            r#"ToolCall: ok_tool {}"#.into(),
                        )
                    } else {
                        Ok("FinalAnswer: done".into())
                    }
                }
            },
            MAX_STEPS,
        )
        .await
        .unwrap();

        let msgs = last_messages.lock().unwrap();
        let combined: String = msgs
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            combined.contains("Observation:"),
            "expected Observation in messages, got: {combined}"
        );
    }

    #[tokio::test]
    async fn loop_returns_error_observation_for_invalid_args() {
        // Tool requires "query" field; we send empty args.
        struct StrictTool;
        impl Tool for StrictTool {
            fn name(&self) -> &str { "strict" }
            fn description(&self) -> &str { "requires query" }
            fn schema(&self) -> Value {
                json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"}}})
            }
            fn execute(&self, _args: &Value) -> Result<String, String> { Ok("ok".into()) }
        }

        let strict = StrictTool;
        let mut tools: HashMap<&str, &dyn Tool> = HashMap::new();
        tools.insert("strict", &strict);

        let calls = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let calls_clone = calls.clone();

        // First call: invalid tool call (missing required field)
        // Second call: should see an Observation error, then return FinalAnswer
        let _ = execute_react_loop(
            vec![json!({"role": "user", "content": "go"})],
            &tools,
            move |_msgs| {
                let mut c = calls_clone.lock().unwrap();
                *c += 1;
                let n = *c;
                async move {
                    if n == 1 {
                        // Missing required "query"
                        Ok::<String, Box<dyn std::error::Error>>(r#"ToolCall: strict {}"#.into())
                    } else {
                        Ok("FinalAnswer: recovered".into())
                    }
                }
            },
            MAX_STEPS,
        )
        .await
        .unwrap();
        // Should not panic; observation error is fed back to the model.
    }

    #[tokio::test]
    async fn loop_hard_stops_at_step_limit() {
        // The model never returns FinalAnswer, so the circuit breaker fires.
        let tools: HashMap<&str, &dyn Tool> = HashMap::new();

        let err = execute_react_loop(
            vec![json!({"role": "user", "content": "loop forever"})],
            &tools,
            |_msgs| async {
                Ok::<String, Box<dyn std::error::Error>>("Thought: still thinking...".into())
            },
            3, // use a small limit for the test
        )
        .await
        .unwrap_err();

        assert_eq!(err.steps_taken, 3);
        assert!(
            err.message.contains("step limit"),
            "expected 'step limit' in error, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn always_failing_tool_returns_error_observation() {
        let fail_tool = AlwaysFailTool;
        let mut tools: HashMap<&str, &dyn Tool> = HashMap::new();
        tools.insert("always_fail", &fail_tool);

        let calls = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let calls_clone = calls.clone();

        let result = execute_react_loop(
            vec![json!({"role": "user", "content": "use always_fail"})],
            &tools,
            move |_msgs| {
                let mut c = calls_clone.lock().unwrap();
                *c += 1;
                let n = *c;
                async move {
                    if n == 1 {
                        Ok::<String, Box<dyn std::error::Error>>(
                            r#"ToolCall: always_fail {}"#.into(),
                        )
                    } else {
                        // Model recovers after seeing the error observation
                        Ok("FinalAnswer: recovered".into())
                    }
                }
            },
            MAX_STEPS,
        )
        .await
        .unwrap();

        assert_eq!(result, "recovered");
    }
}
