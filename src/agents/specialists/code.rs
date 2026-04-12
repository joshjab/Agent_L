use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::agents::{Agent, schema::ParseError};
use crate::app::AppEvent;
use crate::prompts;
use crate::tools::claude_code::ClaudeCodeInvoker;

/// Whether a code task is a small self-contained script or a change to an
/// existing project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskScope {
    /// A short, self-contained script or snippet — runs in a temp sandbox.
    OneOff,
    /// A change to an ongoing project — runs in the project working directory.
    Project,
}

/// JSON schema enforced on Ollama's response for scope classification.
fn scope_schema() -> Value {
    json!({
        "type": "object",
        "required": ["scope"],
        "properties": {
            "scope": {
                "type": "string",
                "enum": ["one_off", "project"]
            }
        }
    })
}

const SCOPE_FALLBACK: &str = "\
You are a code task classifier. Given a description of a coding task, decide \
whether it is a self-contained one-off script/snippet that can run in a fresh \
temporary directory, or whether it requires modifying an existing project.

Rules:
- one_off: write a script, generate a snippet, create a standalone file, \
  \"make a function that...\", \"write a program that...\"
- project: add a feature, fix a bug, refactor, modify an existing file, \
  \"add X to the project\", \"change how Y works\"

Output exactly one JSON object matching the schema.";

/// Asks Ollama to classify a code task as `one_off` or `project`.
pub struct ScopeDetector {
    pub model: String,
}

impl ScopeDetector {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

impl Agent for ScopeDetector {
    type Output = TaskScope;

    fn prompt(&self, context: &[Value], error_feedback: Option<&ParseError>) -> Value {
        // context[0] is the task description the Code specialist passes in
        let scope_prompt = prompts::load("code_scope", SCOPE_FALLBACK);
        let mut messages = vec![json!({ "role": "system", "content": scope_prompt })];
        messages.extend_from_slice(context);

        if let Some(err) = error_feedback {
            messages.push(json!({
                "role": "user",
                "content": format!(
                    "Your previous response was invalid. Error: {}. \
                     Please output a valid JSON object matching the schema.",
                    err.message
                )
            }));
        }

        json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "think": false,
            "format": scope_schema()
        })
    }

    fn parse(&self, response: &str) -> Result<TaskScope, ParseError> {
        let envelope: Value = serde_json::from_str(response).map_err(|e| ParseError {
            message: format!("Ollama response is not valid JSON: {e} (raw: {response:?})"),
        })?;

        let content = envelope["message"]["content"]
            .as_str()
            .ok_or_else(|| ParseError {
                message: format!(
                    "Ollama response missing message.content string (got: {envelope})"
                ),
            })?;

        #[derive(Deserialize)]
        struct ScopePayload {
            scope: TaskScope,
        }

        let payload: ScopePayload = serde_json::from_str(content).map_err(|e| ParseError {
            message: format!("scope JSON is invalid: {e} (content: {content:?})"),
        })?;

        Ok(payload.scope)
    }
}

/// Quick keyword-based scope classifier that runs before the Ollama call.
///
/// Returns `Some(scope)` when the task has clear signals, or `None` to fall
/// through to Ollama classification.
///
/// - Project signals: references to specific file paths (`src/`, `.rs`, etc.)
///   or phrases like "in the project / codebase / repo".
/// - One-off signals: explicit requests to write/create a new standalone script
///   or function with no file-path reference.
fn classify_scope_from_keywords(task: &str) -> Option<TaskScope> {
    let lower = task.to_lowercase();

    let project_signals: &[&str] = &[
        "src/",
        "tests/",
        ".rs",
        ".toml",
        ".md",
        ".json",
        ".yaml",
        ".yml",
        "in this project",
        "in the project",
        "in the codebase",
        "in the repo",
        "this file",
        "that file",
    ];
    if project_signals.iter().any(|s| lower.contains(s)) {
        return Some(TaskScope::Project);
    }

    let one_off_signals: &[&str] = &[
        "write a script",
        "write a bash",
        "write a python",
        "write a program",
        "create a script",
        "generate a script",
        "make a script",
        "write a function that",
        "write a function to",
        "create a function",
        "make a function",
    ];
    if one_off_signals.iter().any(|s| lower.contains(s)) {
        return Some(TaskScope::OneOff);
    }

    None
}

/// Message shown to the user when a project-scope task is detected.
/// Direct file editing is not yet supported — the permission relay (M8) is
/// needed before we can safely let Claude modify project files.
const PROJECT_LIMITATION_MSG: &str = "\
This looks like a task that requires editing existing project files directly. \
Direct file editing is not supported yet — the Code specialist currently runs \
one-off scripts in a temporary sandbox only.\n\n\
To use it now, try rephrasing as a standalone script. For example:\n\
  \"Write a script that reads src/agents/orchestrator.rs and prints the system prompt\"\n\
  \"Write a Python script that searches .rs files for a pattern and prints matches\"\n\n\
Full project editing will be available once the permission relay is implemented (M8).";

/// Executes a code task by delegating to the `claude` CLI.
///
/// First classifies the task scope via Ollama, then:
/// - [`TaskScope::OneOff`]: runs `claude` in a temporary sandbox directory and
///   returns the full output.
/// - [`TaskScope::Project`]: runs `claude` in `working_dir` and streams each
///   output line as an [`AppEvent::Token`].
pub struct CodeSpecialist {
    /// Ollama model used for scope classification.
    pub model: String,
    /// Ollama base URL used for scope classification.
    pub chat_url: String,
    /// Working directory for `Project`-scoped tasks (used once M8 permission
    /// relay enables direct file editing).
    #[allow(dead_code)]
    pub working_dir: PathBuf,
    /// The invoker used to run the `claude` CLI.
    pub invoker: ClaudeCodeInvoker,
}

impl CodeSpecialist {
    /// Code may write files or run subprocesses — not safe to run in parallel
    /// with other specialists. Used by M8 parallel runner.
    #[allow(dead_code)]
    pub fn concurrency_safe() -> bool {
        false
    }

    pub fn new(
        model: impl Into<String>,
        chat_url: impl Into<String>,
        working_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            model: model.into(),
            chat_url: chat_url.into(),
            working_dir: working_dir.into(),
            invoker: ClaudeCodeInvoker::new(),
        }
    }

    /// Run the code task: detect scope, then execute via the appropriate path.
    ///
    /// Returns the full output string (one-off) or an empty string (project —
    /// output was streamed as tokens). Returns `Err` if scope detection or
    /// execution fails.
    pub async fn run(
        &self,
        task: &str,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Result<String, String> {
        // Step 1: classify the task scope.
        // Try the keyword heuristic first; fall back to Ollama if ambiguous.
        let scope = if let Some(s) = classify_scope_from_keywords(task) {
            s
        } else {
            let detector = ScopeDetector::new(&self.model);
            let context = vec![json!({ "role": "user", "content": task })];
            let chat_url = self.chat_url.clone();

            crate::agents::call_with_retry(
                &detector,
                &context,
                |req| {
                    let url = chat_url.clone();
                    async move { crate::ollama::post_json(&url, req).await }
                },
                3,
            )
            .await
            .map_err(|e| format!("scope detection failed: {e}"))?
        };

        // Notify the UI which scope was detected before executing.
        let _ = tx.send(AppEvent::ScopeDecision(scope.clone()));

        match scope {
            TaskScope::OneOff => {
                // Run in a fresh temporary directory; clean up when done.
                let tmp =
                    tempfile::tempdir().map_err(|e| format!("failed to create temp dir: {e}"))?;
                let output = self
                    .invoker
                    .run(task, tmp.path())
                    .await
                    .map_err(|e| e.message)?;
                // `tmp` drops here, deleting the sandbox directory.
                Ok(output)
            }
            TaskScope::Project => {
                // Direct file editing is not yet supported — the permission
                // relay (M8) is needed first. Return the limitation message as
                // the output so run_plan() can present it via the synthesis layer.
                Ok(PROJECT_LIMITATION_MSG.to_string())
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> ScopeDetector {
        ScopeDetector::new("test-model")
    }

    fn ollama_envelope(scope: &str) -> String {
        json!({
            "message": {
                "role": "assistant",
                "content": format!(r#"{{"scope":"{scope}"}}"#)
            }
        })
        .to_string()
    }

    // ── ScopeDetector::prompt() ──────────────────────────────────────────────

    #[test]
    fn prompt_sets_model_stream_false_and_format() {
        let req = agent().prompt(&[], None);
        assert_eq!(req["model"], "test-model");
        assert_eq!(req["stream"], false);
        assert_eq!(req["think"], false);
        assert!(
            req["format"].is_object(),
            "format should be a JSON schema object"
        );
    }

    #[test]
    fn prompt_first_message_is_system_prompt() {
        let req = agent().prompt(&[], None);
        let first = &req["messages"][0];
        assert_eq!(first["role"], "system");
        let content = first["content"].as_str().unwrap();
        assert!(
            content.contains("one_off"),
            "system prompt should mention one_off"
        );
        assert!(
            content.contains("project"),
            "system prompt should mention project"
        );
    }

    #[test]
    fn prompt_includes_context_after_system_message() {
        let ctx = vec![json!({"role": "user", "content": "write a python script"})];
        let req = agent().prompt(&ctx, None);
        let messages = req["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2); // system + 1 context turn
        assert_eq!(messages[1]["content"], "write a python script");
    }

    #[test]
    fn prompt_appends_error_feedback_on_retry() {
        let err = ParseError {
            message: "scope field missing".into(),
        };
        let req = agent().prompt(&[], Some(&err));
        let messages = req["messages"].as_array().unwrap();
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "user");
        let content = last["content"].as_str().unwrap();
        assert!(content.contains("scope field missing"));
        assert!(content.contains("invalid"));
    }

    // ── ScopeDetector::parse() ───────────────────────────────────────────────

    #[test]
    fn parse_one_off_returns_one_off() {
        let result = agent().parse(&ollama_envelope("one_off")).unwrap();
        assert_eq!(result, TaskScope::OneOff);
    }

    #[test]
    fn parse_project_returns_project() {
        let result = agent().parse(&ollama_envelope("project")).unwrap();
        assert_eq!(result, TaskScope::Project);
    }

    #[test]
    fn parse_fails_on_invalid_json() {
        let err = agent().parse("not json").unwrap_err();
        assert!(err.message.contains("not valid JSON"));
    }

    #[test]
    fn parse_fails_when_message_content_missing() {
        let raw = json!({ "message": { "role": "assistant" } }).to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(err.message.contains("missing message.content"));
    }

    #[test]
    fn parse_fails_on_unknown_scope_value() {
        let raw = json!({
            "message": { "content": r#"{"scope":"unknown_value"}"# }
        })
        .to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(err.message.contains("scope JSON is invalid"));
    }

    #[test]
    fn parse_fails_when_scope_field_missing() {
        let raw = json!({ "message": { "content": r#"{"other":"field"}"# } }).to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(err.message.contains("scope JSON is invalid"));
    }

    // ── TaskScope serialization ──────────────────────────────────────────────

    #[test]
    fn task_scope_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TaskScope::OneOff).unwrap(),
            r#""one_off""#
        );
        assert_eq!(
            serde_json::to_string(&TaskScope::Project).unwrap(),
            r#""project""#
        );
    }

    #[test]
    fn task_scope_deserializes_snake_case() {
        let v: TaskScope = serde_json::from_value(json!("one_off")).unwrap();
        assert_eq!(v, TaskScope::OneOff);
    }

    // ── call_with_retry integration (mock HTTP) ──────────────────────────────

    #[tokio::test]
    async fn retry_on_invalid_response_then_succeeds() {
        use crate::agents::call_with_retry;
        use std::sync::{Arc, Mutex};

        let calls = Arc::new(Mutex::new(0u32));
        let calls_clone = calls.clone();

        let result = call_with_retry(
            &agent(),
            &[json!({"role": "user", "content": "write a bash script"})],
            move |_req| {
                let calls = calls_clone.clone();
                async move {
                    let n = {
                        let mut c = calls.lock().unwrap();
                        *c += 1;
                        *c
                    };
                    // First call returns invalid JSON; second returns valid.
                    let body = if n == 1 {
                        r#"{"message":{"content":"not a scope object"}}"#.to_string()
                    } else {
                        ollama_envelope("one_off")
                    };
                    Ok::<String, Box<dyn std::error::Error>>(body)
                }
            },
            3,
        )
        .await
        .unwrap();

        assert_eq!(result, TaskScope::OneOff);
        assert_eq!(*calls.lock().unwrap(), 2, "should have taken 2 attempts");
    }

    #[tokio::test]
    async fn exhausted_retries_returns_agent_error() {
        use crate::agents::call_with_retry;

        let err = call_with_retry(
            &agent(),
            &[],
            |_req| async {
                Ok::<String, Box<dyn std::error::Error>>(
                    r#"{"message":{"content":"{}"}}"#.to_string(),
                )
            },
            3,
        )
        .await
        .unwrap_err();

        assert_eq!(err.attempts, 3);
    }

    // ── CodeSpecialist ───────────────────────────────────────────────────────

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a mock Ollama response that returns a given scope.
    fn scope_response(scope: &str) -> String {
        json!({
            "message": {
                "role": "assistant",
                "content": format!(r#"{{"scope":"{scope}"}}"#)
            }
        })
        .to_string()
    }

    /// A `CodeSpecialist` wired to use `echo` as the claude binary so tests
    /// don't need the real CLI installed.
    fn echo_specialist(chat_url: &str) -> CodeSpecialist {
        let mut s = CodeSpecialist::new("test-model", chat_url, std::env::temp_dir());
        s.invoker = ClaudeCodeInvoker::with_command("echo", vec![]);
        s
    }

    /// `sh -c` specialist — used once M8 enables the project streaming path.
    #[allow(dead_code)]
    fn sh_specialist(chat_url: &str) -> CodeSpecialist {
        let mut s = CodeSpecialist::new("test-model", chat_url, std::env::temp_dir());
        s.invoker = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);
        s
    }

    // ── one-off path ─────────────────────────────────────────────────────────

    /// Ollama says one_off → specialist runs `echo <task>`, returns the output.
    #[tokio::test]
    async fn one_off_returns_output() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scope_response("one_off")))
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        let specialist = echo_specialist(&url);
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = specialist.run("hello task", tx).await.unwrap();
        assert!(result.contains("hello task"), "got: {result:?}");
    }

    /// One-off path creates and then cleans up a temp directory (the invoker
    /// should not receive the project working_dir — it gets its own tempdir).
    /// We verify the task ran in some temp-like dir by checking `sh -c "pwd"`
    /// returns a temp path, not the specialist's working_dir.
    #[tokio::test]
    async fn one_off_runs_in_temp_dir_not_project_dir() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scope_response("one_off")))
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        // Use a distinctive non-temp project dir so we can tell them apart.
        let project_dir = std::env::current_dir().unwrap();
        let mut s = CodeSpecialist::new("test-model", &url, project_dir.clone());
        s.invoker = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = s.run("pwd", tx).await.unwrap();

        // The output should NOT be the project directory — it's a temp dir.
        let output_dir = result.trim();
        assert_ne!(
            output_dir,
            project_dir.to_str().unwrap(),
            "one-off should run in a temp dir, not the project dir"
        );
    }

    // ── project path ─────────────────────────────────────────────────────────

    /// Ollama says project → specialist returns a limitation message (not subprocess output).
    #[tokio::test]
    async fn project_sends_limitation_message_not_subprocess_output() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scope_response("project")))
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        // Bad binary — subprocess must not be called.
        let mut s = CodeSpecialist::new("test-model", &url, std::env::temp_dir());
        s.invoker = ClaudeCodeInvoker::with_command("/nonexistent/binary", vec![]);

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = s.run("some project task", tx).await.unwrap();

        assert!(
            result.contains("not supported") || result.contains("limitation"),
            "expected limitation message in return value, got: {result:?}"
        );
    }

    // ── sad paths ────────────────────────────────────────────────────────────

    /// If scope detection fails (Ollama down), run() returns Err.
    #[tokio::test]
    async fn scope_detection_failure_returns_err() {
        // Use a port with nothing listening.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}/api/chat");
        let specialist = echo_specialist(&url);
        let (tx, _rx) = mpsc::unbounded_channel();
        // Use an ambiguous task so the heuristic returns None and Ollama is called.
        let result = specialist.run("do something useful", tx).await;
        assert!(result.is_err(), "should fail when Ollama is unreachable");
    }

    // ── keyword heuristic ────────────────────────────────────────────────────

    /// A task mentioning a src/ file path should be detected as Project without
    /// needing an Ollama call.
    #[test]
    fn heuristic_file_path_is_project() {
        assert_eq!(
            classify_scope_from_keywords("add a comment to src/agents/orchestrator.rs"),
            Some(TaskScope::Project)
        );
    }

    /// A task asking to "write a script" with no file-path reference is OneOff.
    #[test]
    fn heuristic_write_script_is_one_off() {
        assert_eq!(
            classify_scope_from_keywords("write a bash script that counts lines"),
            Some(TaskScope::OneOff)
        );
    }

    /// An ambiguous task (no strong signals) should return None so Ollama decides.
    #[test]
    fn heuristic_ambiguous_returns_none() {
        assert_eq!(classify_scope_from_keywords("explain recursion"), None);
    }

    /// A task referencing a .toml file is Project.
    #[test]
    fn heuristic_toml_file_is_project() {
        assert_eq!(
            classify_scope_from_keywords("add tempfile to Cargo.toml"),
            Some(TaskScope::Project)
        );
    }

    // ── project limitation message ───────────────────────────────────────────

    /// When scope is Project the specialist returns a limitation message in the
    /// output string (no subprocess is spawned, no Token event emitted directly).
    #[tokio::test]
    async fn project_scope_sends_limitation_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scope_response("project")))
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        // Use a bad binary — if the subprocess were called it would error.
        let mut s = CodeSpecialist::new("test-model", &url, std::env::temp_dir());
        s.invoker = ClaudeCodeInvoker::with_command("/nonexistent/binary", vec![]);

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = s.run("modify src/main.rs", tx).await.unwrap();

        assert!(
            result.contains("not supported") || result.contains("limitation"),
            "expected limitation message in return value, got: {result:?}"
        );
    }

    /// Project scope detected via keyword heuristic also returns a limitation
    /// message and does not call the subprocess.
    #[tokio::test]
    async fn project_scope_via_heuristic_sends_limitation_message() {
        // No mock server needed — heuristic should short-circuit before Ollama.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = format!("http://127.0.0.1:{port}/api/chat");
        let mut s = CodeSpecialist::new("test-model", &url, std::env::temp_dir());
        s.invoker = ClaudeCodeInvoker::with_command("/nonexistent/binary", vec![]);

        let (tx, _rx) = mpsc::unbounded_channel();
        // This task has "src/" in it — heuristic should classify as Project
        // without hitting Ollama (which isn't running on the dead port).
        let result = s.run("add a comment to src/config.rs", tx).await.unwrap();

        assert!(
            result.contains("not supported") || result.contains("limitation"),
            "should return a limitation message, got: {result:?}"
        );
    }

    /// If the claude invocation fails (binary exits non-zero), run() returns Err.
    #[tokio::test]
    async fn failing_invocation_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scope_response("one_off")))
            .mount(&server)
            .await;

        let url = format!("{}/api/chat", server.uri());
        // "sh -c 'exit 1'" will fail.
        let mut s = CodeSpecialist::new("test-model", &url, std::env::temp_dir());
        s.invoker = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);

        let (tx, _rx) = mpsc::unbounded_channel();
        let result = s.run("exit 1", tx).await;
        assert!(result.is_err(), "should propagate process error");
    }
}
