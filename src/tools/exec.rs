//! `execute_command` tool — resource-limited command execution.
//!
//! Runs shell commands with a 30-second timeout, working directory pinned to
//! the workspace root, combined stdout+stderr capped at 32 KB, and no shell
//! interpretation. Returns exit code, stdout, stderr, and a truncation flag.

use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::warn;

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};

/// Maximum formatted output size in bytes before truncation.
/// Applied to the full result string (headers + stdout + stderr).
const OUTPUT_CAP: usize = 32 * 1024; // 32 KB

/// Tool that executes a command with resource limits.
///
/// The LLM can use this to run build commands, tests, formatters, linters, etc.
/// All commands are pinned to the workspace root and cannot escape via `cd` or
/// shell metacharacters (no shell is involved — arguments are passed directly).
pub struct ExecuteCommandTool;

impl Tool for ExecuteCommandTool {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Execute a command in the project workspace with resource limits. \
         The command runs with a 30-second timeout, working directory pinned \
         to the workspace root, combined stdout+stderr output capped at 32 KB, \
         and no shell interpretation (use command + args array — not a shell \
         string). Returns exit code, stdout, stderr, and a truncation notice \
         if the output exceeds the cap."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Command to execute (e.g., 'cargo', 'python', 'ls')"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments to pass to the command (optional)"
                }
            },
            "required": ["command"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            // ------------------------------------------------------------------
            // Extract and validate inputs
            // ------------------------------------------------------------------
            let command_str = match input.get("command").and_then(Value::as_str) {
                Some(c) => c.to_string(),
                None => return Ok(ToolResult::error("missing required 'command' parameter")),
            };

            if command_str.is_empty() {
                return Ok(ToolResult::error("command must not be empty"));
            }

            let args: Vec<String> = input
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // ------------------------------------------------------------------
            // Build the command
            // ------------------------------------------------------------------
            let mut cmd = Command::new(&command_str);
            cmd.args(&args)
                .current_dir(&ctx.workspace_root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::null())
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", std::env::var("HOME").unwrap_or_default());

            // ------------------------------------------------------------------
            // Spawn the child process
            // ------------------------------------------------------------------
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return Ok(ToolResult::error(format!("failed to spawn command: {e}"))),
            };

            // ------------------------------------------------------------------
            // Take output handles and read concurrently
            // ------------------------------------------------------------------
            let mut stdout_pipe = match child.stdout.take() {
                Some(p) => p,
                None => return Ok(ToolResult::error("internal error: stdout pipe missing")),
            };
            let mut stderr_pipe = match child.stderr.take() {
                Some(p) => p,
                None => return Ok(ToolResult::error("internal error: stderr pipe missing")),
            };

            // Read stdout and stderr in separate tasks so they drain concurrently
            // while the process runs, preventing pipe buffer deadlocks.
            let stdout_task = tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = stdout_pipe.read_to_end(&mut buf).await;
                buf
            });
            let stderr_task = tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = stderr_pipe.read_to_end(&mut buf).await;
                buf
            });

            // ------------------------------------------------------------------
            // Wait with 30-second timeout
            // ------------------------------------------------------------------
            let wait_result = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;

            match wait_result {
                Ok(Ok(status)) => {
                    // Process exited normally within the timeout.
                    let stdout_bytes = stdout_task
                        .await
                        .inspect_err(|e| warn!("stdout reader task panicked: {e}"))
                        .unwrap_or_default();
                    let stderr_bytes = stderr_task
                        .await
                        .inspect_err(|e| warn!("stderr reader task panicked: {e}"))
                        .unwrap_or_default();

                    let exit_code = status.code().unwrap_or(-1);
                    let stdout_str = String::from_utf8_lossy(&stdout_bytes);
                    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

                    // Build the structured output string.
                    let mut output = format!("Exit code: {exit_code}\n");
                    output.push_str("--- stdout ---\n");
                    if stdout_str.is_empty() {
                        output.push_str("(empty)");
                    } else {
                        output.push_str(&stdout_str);
                    }
                    output.push('\n');
                    output.push_str("--- stderr ---\n");
                    if stderr_str.is_empty() {
                        output.push_str("(empty)");
                    } else {
                        output.push_str(&stderr_str);
                    }
                    output.push('\n');

                    // Cap combined output at 32 KB, truncating at a UTF-8
                    // character boundary.
                    let truncated = output.len() > OUTPUT_CAP;
                    if truncated {
                        // Keep OUTPUT_CAP bytes, then add the truncation note.
                        let cap = (0..=OUTPUT_CAP)
                            .rev()
                            .find(|&i| output.is_char_boundary(i))
                            .unwrap_or(0);
                        output.truncate(cap);
                        output.push_str("--- (TRUNCATED at 32 KB) ---\n");
                    }

                    let trimmed_len = output.trim_end().len();
                    output.truncate(trimmed_len);
                    Ok(ToolResult::ok(output))
                }
                Ok(Err(e)) => {
                    // wait() returned an I/O error.
                    stdout_task.abort();
                    stderr_task.abort();
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    Ok(ToolResult::error(format!("command failed: {e}")))
                }
                Err(_elapsed) => {
                    // The 30-second timeout expired.
                    stdout_task.abort();
                    stderr_task.abort();
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    Ok(ToolResult::error(
                        "command timed out after 30s and was killed",
                    ))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::traits::ToolContext;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::hashing::state::AnchorState;
    use serde_json::json;

    /// Build a minimal `ToolContext` pointing to the given workspace root.
    fn test_context() -> ToolContext {
        ToolContext {
            workspace_root: PathBuf::from("/tmp"),
            anchor_state: Arc::new(Mutex::new(AnchorState::new())),
        }
    }

    // -----------------------------------------------------------------------
    // Basic execution
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn echo_hello() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool
            .execute(json!({"command": "echo", "args": ["hello"]}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("Exit code: 0"));
        assert!(result.content.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sleep_short() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool
            .execute(json!({"command": "sleep", "args": ["2"]}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("Exit code: 0"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn args_passthrough() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool
            .execute(json!({"command": "echo", "args": ["hello", "world"]}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("hello world"));
    }

    // -----------------------------------------------------------------------
    // Error handling
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn missing_command_parameter() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool.execute(json!({}), &ctx).await.unwrap();

        assert!(result.is_error);
        assert!(
            result.content.contains("missing required 'command'"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn empty_command() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool.execute(json!({"command": ""}), &ctx).await.unwrap();

        assert!(result.is_error);
        assert!(
            result.content.contains("command must not be empty"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn command_not_found() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool
            .execute(json!({"command": "nonexistent_cmd_xyz_98765"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(
            result.content.contains("failed to spawn command"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn command_is_not_a_string() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool.execute(json!({"command": 42}), &ctx).await.unwrap();

        assert!(result.is_error);
        assert!(
            result.content.contains("missing required 'command'"),
            "got: {}",
            result.content
        );
    }

    // -----------------------------------------------------------------------
    // Output truncation
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn output_truncation() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();

        // Use `dd` to produce exactly 40000 bytes of output to stdout.
        // This exceeds the 32 KB cap, so the result should include the
        // truncation notice.
        let result = tool
            .execute(
                json!({"command": "dd", "args": ["if=/dev/zero", "bs=1", "count=40000"]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            result.content.contains("TRUNCATED at 32 KB"),
            "expected truncation notice in output, got:\n{}",
            result.content
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn short_output_not_truncated() {
        let tool = ExecuteCommandTool;
        let ctx = test_context();
        let result = tool
            .execute(json!({"command": "echo", "args": ["short"]}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            !result.content.contains("TRUNCATED"),
            "short output should not be truncated, got:\n{}",
            result.content
        );
    }

    #[test]
    fn tool_metadata() {
        let tool = ExecuteCommandTool;
        assert_eq!(tool.name(), "execute_command");
        assert!(!tool.is_read_only());
        let schema = tool.parameters_schema();
        assert!(schema
            .get("required")
            .and_then(|v| v.as_array())
            .map_or(false, |arr| arr
                .iter()
                .any(|v| v.as_str() == Some("command"))));
    }
}
