pub mod claude_code;
pub mod executor;
pub mod search_tools;

use serde_json::Value;

/// A tool that a specialist can invoke during the ReAct loop.
/// Used by M7 (Search) and M8 (Shell) specialists.
#[allow(dead_code)]
///
/// All tool implementations must be `Send + Sync` so they can be called from
/// async contexts without holding a mutex.
pub trait Tool: Send + Sync {
    /// Short, unique name used to identify this tool (e.g., `"web_search"`).
    fn name(&self) -> &str;

    /// Human-readable description forwarded to the model so it can decide
    /// whether to use this tool.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's argument object. The executor validates call
    /// args against this schema before invoking `execute`.
    fn schema(&self) -> Value;

    /// Execute the tool with validated `args` and return the observation.
    /// Returns `Err` on failure (error message forwarded back to the model).
    fn execute(&self, args: &Value) -> Result<String, String>;
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod test_tools {
    use super::*;
    use serde_json::json;

    /// A mock tool that always succeeds, returning `"mock result"`.
    pub struct AlwaysOkTool {
        pub name: &'static str,
    }

    impl Tool for AlwaysOkTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "always succeeds"
        }
        fn schema(&self) -> Value {
            json!({"type": "object", "properties": {"input": {"type": "string"}}, "required": []})
        }
        fn execute(&self, _args: &Value) -> Result<String, String> {
            Ok("mock result".into())
        }
    }

    /// A mock tool that always returns an error.
    pub struct AlwaysFailTool;

    impl Tool for AlwaysFailTool {
        fn name(&self) -> &str {
            "always_fail"
        }
        fn description(&self) -> &str {
            "always fails"
        }
        fn schema(&self) -> Value {
            json!({"type": "object", "properties": {}, "required": []})
        }
        fn execute(&self, _args: &Value) -> Result<String, String> {
            Err("tool failure".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_tools::{AlwaysFailTool, AlwaysOkTool};

    #[test]
    fn always_ok_tool_name_and_execute() {
        let t = AlwaysOkTool { name: "ok" };
        assert_eq!(t.name(), "ok");
        assert_eq!(t.execute(&serde_json::json!({})).unwrap(), "mock result");
    }

    #[test]
    fn always_fail_tool_returns_err() {
        let t = AlwaysFailTool;
        assert!(t.execute(&serde_json::json!({})).is_err());
    }
}
