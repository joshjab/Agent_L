use std::collections::HashMap;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Tool;
use crate::app::AppEvent;

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
        write!(
            f,
            "executor error after {} steps: {}",
            self.steps_taken, self.message
        )
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
        Some(ReActStep::ToolCall {
            name: name.to_string(),
            args,
        })
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
            if let Some(name) = field.as_str()
                && args.get(name).is_none()
            {
                return Err(format!("missing required field: '{name}'"));
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
    event_tx: Option<mpsc::UnboundedSender<AppEvent>>,
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
            .map_err(|e| ExecutorError {
                message: e.to_string(),
                steps_taken: step + 1,
            })?;

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
                    // Notify observers that a tool is about to execute.
                    if let Some(tx) = &event_tx {
                        let _ = tx.send(AppEvent::ToolCall {
                            name: name.clone(),
                            args: args.clone(),
                        });
                    }

                    let obs = match tools.get(name.as_str()) {
                        Some(tool) => match validate_args(&tool.schema(), &args) {
                            Ok(()) => match tool.execute(&args) {
                                Ok(result) => {
                                    // Notify observers of the tool result.
                                    if let Some(tx) = &event_tx {
                                        let _ = tx.send(AppEvent::ToolResult {
                                            name: name.clone(),
                                            result: result.clone(),
                                        });
                                    }
                                    let enriched = build_observation(0, &result);
                                    format!("Observation: {enriched}")
                                }
                                Err(e) => {
                                    if let Some(tx) = &event_tx {
                                        let _ = tx.send(AppEvent::ToolResult {
                                            name: name.clone(),
                                            result: e.clone(),
                                        });
                                    }
                                    let enriched = build_observation(1, &e);
                                    format!("Observation: {enriched}")
                                }
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

        // A FinalAnswer is only accepted when no tool calls were made in this
        // same response. If the model emits both a ToolCall and a FinalAnswer
        // in one turn, the FinalAnswer is premature (the model hasn't yet seen
        // the search result) and must be discarded. The observation is injected
        // and the model will produce a grounded FinalAnswer in the next round.
        if observations.is_empty()
            && let Some(ans) = final_answer
        {
            return Ok(ans);
        }

        // Append all tool observations as user messages for the next iteration.
        // The FinalAnswer reminder is injected into each observation so the model
        // sees it immediately before its next turn — the system-prompt instruction
        // alone is often ignored by smaller models after a tool result.
        for obs in observations {
            // Quote the first snippet back to the model so it can't substitute
            // a different fact from training knowledge.
            let snippet_reminder = extract_first_snippet(&obs)
                .map(|s| format!("\nThe search result says: \"{s}\" — your answer MUST use this."))
                .unwrap_or_default();

            let content = format!(
                "{obs}{snippet_reminder}\n\n\
                 IMPORTANT: The Observation above is the current, authoritative answer. \
                 Your FinalAnswer MUST copy the exact names, numbers, and facts from the \
                 Observation. Do NOT use your training knowledge. Do NOT change or override \
                 the Observation content, even if it contradicts what you believe.\n\n\
                 Your next output MUST be:\n\
                 FinalAnswer: <answer copied directly from the Observation, with source URL>"
            );
            messages.push(json!({"role": "user", "content": content}));
        }
    }

    Err(ExecutorError {
        message: format!("reached step limit ({max_steps}) without a FinalAnswer"),
        steps_taken: max_steps,
    })
}

/// Extract the first `Snippet: <text>` line from a formatted observation.
/// Used to quote the key search finding back to the model in the injection
/// message so it cannot be overridden by training knowledge.
fn extract_first_snippet(obs: &str) -> Option<&str> {
    for line in obs.lines() {
        if let Some(snippet) = line.strip_prefix("Snippet: ") {
            return Some(snippet.trim());
        }
    }
    None
}

/// Build an observation string from a tool result, enriched with exit-code and
/// semantic-warning metadata for the ReAct loop.
///
/// Format:
/// ```text
/// [exit:<code>] <output (trimmed to 2000 chars)>
/// [WARNING: output contains error/failure indicators]   ← only if triggered
/// ```
///
/// The warning is appended whenever the output contains any of the strings
/// `"error:"`, `"failed:"`, `"panic:"`, or `"WARN:"` — even when `exit_code`
/// is 0. This prevents the model from claiming success on a command that
/// printed errors but still exited cleanly.
pub fn build_observation(exit_code: i32, output: &str) -> String {
    const MAX_OUTPUT: usize = 2000;
    let trimmed = if output.len() > MAX_OUTPUT {
        &output[..MAX_OUTPUT]
    } else {
        output
    };

    let warning_keywords = ["error:", "failed:", "panic:", "WARN:"];
    let has_warning = warning_keywords.iter().any(|kw| output.contains(kw));

    let mut obs = format!("[exit:{exit_code}] {trimmed}");
    if has_warning {
        obs.push_str("\n[WARNING: output contains error/failure indicators]");
    }
    obs
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_tools::{AlwaysFailTool, AlwaysOkTool};
    use serde_json::json;

    // ── extract_first_snippet ─────────────────────────────────────────────────

    #[test]
    fn extract_first_snippet_returns_snippet_line() {
        let obs = "Title: Foo\nURL: https://example.com\nSnippet: Donald Trump is president.";
        assert_eq!(
            extract_first_snippet(obs),
            Some("Donald Trump is president.")
        );
    }

    #[test]
    fn extract_first_snippet_returns_none_when_absent() {
        let obs = "Title: Foo\nURL: https://example.com";
        assert_eq!(extract_first_snippet(obs), None);
    }

    // ── build_observation ────────────────────────────────────────────────────

    #[test]
    fn observation_includes_exit_code() {
        let obs = build_observation(1, "some output");
        assert!(obs.starts_with("[exit:1]"), "got: {obs}");
    }

    #[test]
    fn observation_trims_long_output() {
        let long = "x".repeat(3000);
        let obs = build_observation(0, &long);
        // [exit:0] prefix + space + up to 2000 chars
        assert!(obs.len() <= "[exit:0] ".len() + 2000 + 100); // +100 for warning line
        assert!(!obs.contains(&"x".repeat(2001)));
    }

    #[test]
    fn observation_adds_warning_for_error_keyword() {
        let obs = build_observation(0, "build succeeded\nerror: missing field");
        assert!(obs.contains("[WARNING:"), "got: {obs}");
    }

    #[test]
    fn observation_adds_warning_for_failed_keyword() {
        let obs = build_observation(0, "failed: step 2");
        assert!(obs.contains("[WARNING:"), "got: {obs}");
    }

    #[test]
    fn observation_adds_warning_for_panic_keyword() {
        let obs = build_observation(0, "thread 'main' panic: index out of bounds");
        assert!(obs.contains("[WARNING:"), "got: {obs}");
    }

    #[test]
    fn observation_adds_warning_for_warn_keyword() {
        let obs = build_observation(0, "WARN: unused variable");
        assert!(obs.contains("[WARNING:"), "got: {obs}");
    }

    #[test]
    fn observation_no_warning_for_clean_output() {
        let obs = build_observation(0, "all tests passed successfully");
        assert!(!obs.contains("[WARNING:"), "got: {obs}");
    }

    #[test]
    fn observation_zero_exit_with_panic_keyword_warns() {
        // exit 0 but panic in output — should still warn
        let obs = build_observation(0, "panic: something went wrong");
        assert!(obs.contains("[WARNING:"), "got: {obs}");
    }

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
        let step = parse_react_line(r#"ToolCall: web_search {"query":"rust lang"}"#).unwrap();
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
            None,
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
                        Ok::<String, Box<dyn std::error::Error>>(r#"ToolCall: ok_tool {}"#.into())
                    } else {
                        Ok("FinalAnswer: got mock result".into())
                    }
                }
            },
            None,
            MAX_STEPS,
        )
        .await
        .unwrap();

        assert_eq!(result, "got mock result");
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn tool_call_takes_priority_over_same_turn_final_answer() {
        // When the model emits ToolCall and FinalAnswer in the SAME response, the
        // ToolCall must be executed and the premature FinalAnswer discarded. The
        // observation is injected, and the model must produce a new FinalAnswer
        // in the next round (which may then use the search result).
        let ok_tool = AlwaysOkTool { name: "ok_tool" };
        let mut tools: HashMap<&str, &dyn Tool> = HashMap::new();
        tools.insert("ok_tool", &ok_tool);

        let calls = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let calls_clone = calls.clone();

        let result = execute_react_loop(
            vec![json!({"role": "user", "content": "use tool then answer"})],
            &tools,
            move |_msgs| {
                let mut c = calls_clone.lock().unwrap();
                *c += 1;
                let n = *c;
                async move {
                    if n == 1 {
                        // Model emits ToolCall AND premature FinalAnswer together
                        Ok::<String, Box<dyn std::error::Error>>(
                            "ToolCall: ok_tool {}\nFinalAnswer: premature answer".into(),
                        )
                    } else {
                        // Second call: model sees observation, gives correct answer
                        Ok("FinalAnswer: answer after seeing observation".into())
                    }
                }
            },
            None,
            MAX_STEPS,
        )
        .await
        .unwrap();

        // The premature FinalAnswer must be discarded; the answer must come from
        // the second round (after the model saw the tool observation).
        assert_eq!(
            result, "answer after seeing observation",
            "premature FinalAnswer (before observation) must be discarded"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            2,
            "model must be called twice (tool call round + answer round)"
        );
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
                        Ok::<String, Box<dyn std::error::Error>>(r#"ToolCall: ok_tool {}"#.into())
                    } else {
                        Ok("FinalAnswer: done".into())
                    }
                }
            },
            None,
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
            fn name(&self) -> &str {
                "strict"
            }
            fn description(&self) -> &str {
                "requires query"
            }
            fn schema(&self) -> Value {
                json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"}}})
            }
            fn execute(&self, _args: &Value) -> Result<String, String> {
                Ok("ok".into())
            }
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
            None,
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
            None,
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
            None,
            MAX_STEPS,
        )
        .await
        .unwrap();

        assert_eq!(result, "recovered");
    }
}
