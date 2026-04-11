use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::app::AppEvent;

/// Errors returned by [`ClaudeCodeInvoker`].
#[derive(Debug)]
pub struct ClaudeCodeError {
    pub message: String,
}

impl std::fmt::Display for ClaudeCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "claude code error: {}", self.message)
    }
}

impl std::error::Error for ClaudeCodeError {}

/// Invokes the `claude` CLI as a subprocess.
///
/// The binary and argument prefix are configurable so tests can substitute
/// `echo`, `sh -c`, etc. without actually needing Claude installed.
pub struct ClaudeCodeInvoker {
    /// The binary to run (default: `"claude"`).
    binary: String,
    /// Arguments prepended before the prompt (default: `["--print"]`).
    args_prefix: Vec<String>,
    /// Maximum time to wait before killing the process.
    timeout: Duration,
}

impl Default for ClaudeCodeInvoker {
    fn default() -> Self {
        Self {
            binary: "claude".into(),
            args_prefix: vec!["--print".into()],
            timeout: Duration::from_secs(120),
        }
    }
}

impl ClaudeCodeInvoker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the binary and args prefix — used in tests to avoid needing
    /// the real `claude` CLI installed.
    #[cfg(test)]
    pub fn with_command(binary: impl Into<String>, args_prefix: Vec<String>) -> Self {
        Self {
            binary: binary.into(),
            args_prefix,
            timeout: Duration::from_secs(5),
        }
    }

    /// Override the timeout — used in tests to trigger the timeout quickly.
    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run the claude CLI non-interactively and return all stdout output.
    ///
    /// Returns `Err` if the process fails to spawn, exits non-zero, or exceeds
    /// the timeout.
    pub async fn run(&self, prompt: &str, working_dir: &Path) -> Result<String, ClaudeCodeError> {
        let mut cmd = Command::new(&self.binary);
        for arg in &self.args_prefix {
            cmd.arg(arg);
        }
        cmd.arg(prompt)
            .current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let fut = async {
            let output = cmd.output().await.map_err(|e| ClaudeCodeError {
                message: format!("failed to spawn process: {e}"),
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ClaudeCodeError {
                    message: format!("process exited with {}: {stderr}", output.status),
                });
            }

            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };

        timeout(self.timeout, fut).await.unwrap_or_else(|_| {
            Err(ClaudeCodeError {
                message: format!("process timed out after {:?}", self.timeout),
            })
        })
    }

    /// Run the claude CLI and stream each line of stdout as an
    /// [`AppEvent::Token`]. Returns `Err` on spawn failure, non-zero exit, or
    /// timeout. Used by the Project path once M8 permission relay is in place.
    #[allow(dead_code)]
    pub async fn run_streaming(
        &self,
        prompt: &str,
        working_dir: &Path,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Result<(), ClaudeCodeError> {
        let mut cmd = Command::new(&self.binary);
        for arg in &self.args_prefix {
            cmd.arg(arg);
        }
        let mut child = cmd
            .arg(prompt)
            .current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ClaudeCodeError {
                message: format!("failed to spawn process: {e}"),
            })?;

        let stdout = child.stdout.take().ok_or_else(|| ClaudeCodeError {
            message: "failed to capture stdout".into(),
        })?;

        let fut = async {
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await.map_err(|e| ClaudeCodeError {
                message: format!("error reading output: {e}"),
            })? {
                let _ = tx.send(AppEvent::Token(line + "\n"));
            }

            let status = child.wait().await.map_err(|e| ClaudeCodeError {
                message: format!("error waiting for process: {e}"),
            })?;

            if !status.success() {
                return Err(ClaudeCodeError {
                    message: format!("process exited with {status}"),
                });
            }

            Ok(())
        };

        timeout(self.timeout, fut).await.unwrap_or_else(|_| {
            Err(ClaudeCodeError {
                message: format!("process timed out after {:?}", self.timeout),
            })
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        std::env::temp_dir()
    }

    // ── run() — happy path ───────────────────────────────────────────────────

    /// `echo` prints its arguments to stdout — verifies we capture output.
    #[tokio::test]
    async fn run_captures_stdout() {
        let inv = ClaudeCodeInvoker::with_command("echo", vec![]);
        let out = inv.run("hello world", &tmp()).await.unwrap();
        assert!(out.contains("hello world"), "got: {out:?}");
    }

    /// Run `sh -c "pwd"` in a specific directory — verifies current_dir is
    /// respected.
    #[tokio::test]
    async fn run_uses_specified_working_dir() {
        let inv = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);
        let dir = tmp().canonicalize().unwrap_or_else(|_| tmp());
        let out = inv.run("pwd", &dir).await.unwrap();
        assert_eq!(out.trim(), dir.to_str().unwrap(), "got: {out:?}");
    }

    // ── run() — sad path ─────────────────────────────────────────────────────

    /// A process that exits with code 1 should return Err.
    #[tokio::test]
    async fn run_returns_err_on_nonzero_exit() {
        let inv = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);
        let result = inv.run("exit 1", &tmp()).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().message.contains("exited with"),
            "error should mention exit status"
        );
    }

    /// Pointing at a binary that does not exist should return Err immediately.
    #[tokio::test]
    async fn run_returns_err_on_missing_binary() {
        let inv = ClaudeCodeInvoker::with_command("/nonexistent/binary/abc", vec![]);
        let result = inv.run("test", &tmp()).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().message.contains("failed to spawn"),
            "error should mention spawn failure"
        );
    }

    /// A process that takes longer than the timeout should return Err.
    #[tokio::test]
    async fn run_timeout_fires() {
        let inv = ClaudeCodeInvoker::with_command("sleep", vec![])
            .with_timeout(Duration::from_millis(50));
        let result = inv.run("10", &tmp()).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().message.contains("timed out"),
            "error should mention timeout"
        );
    }

    // ── run_streaming() — happy path ─────────────────────────────────────────

    /// `sh -c "printf 'line1\nline2\n'"` produces two lines — each should
    /// arrive as a separate Token event.
    #[tokio::test]
    async fn run_streaming_sends_tokens() {
        let inv = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        inv.run_streaming("printf 'line1\\nline2\\n'", &tmp(), tx)
            .await
            .unwrap();

        let tokens: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|e| {
                if let AppEvent::Token(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();

        assert!(!tokens.is_empty(), "expected at least one token");
        let combined = tokens.join("");
        assert!(combined.contains("line1"), "got: {combined:?}");
        assert!(combined.contains("line2"), "got: {combined:?}");
    }

    // ── run_streaming() — sad path ───────────────────────────────────────────

    /// Non-zero exit from the streaming path should return Err.
    #[tokio::test]
    async fn run_streaming_returns_err_on_nonzero_exit() {
        let inv = ClaudeCodeInvoker::with_command("sh", vec!["-c".into()]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = inv.run_streaming("exit 1", &tmp(), tx).await;
        assert!(result.is_err(), "expected Err for non-zero exit");
    }

    /// A streaming process that exceeds the timeout should return Err.
    #[tokio::test]
    async fn run_streaming_timeout_fires() {
        let inv = ClaudeCodeInvoker::with_command("sleep", vec![])
            .with_timeout(Duration::from_millis(50));
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = inv.run_streaming("10", &tmp(), tx).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().message.contains("timed out"),
            "error should mention timeout"
        );
    }
}
