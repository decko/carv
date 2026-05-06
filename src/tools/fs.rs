//! `read_file` tool — read file contents with hash-anchored line references.
//!
//! Resolves paths relative to the workspace root, canonicalizes when possible,
//! and returns each line prefixed with its stable anchor word.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};

/// Tool that reads a file and returns its contents with stable anchor identifiers.
///
/// Anchors are deterministic word-based identifiers (not line numbers) so they
/// remain stable across edits. The LLM uses these anchor words when referencing
/// specific lines in subsequent edit operations.
pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file from the project workspace. \
         Returns file contents with stable anchor identifiers for each line. \
         The LLM can reference these anchors in edit operations."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read, relative to the project root or absolute"
                }
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            // Extract the "path" parameter.
            let path_str = match input.get("path").and_then(Value::as_str) {
                Some(p) => p,
                None => return Ok(ToolResult::error("missing required 'path' parameter")),
            };

            // Resolve relative paths against the workspace root.
            let resolved = if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                ctx.workspace_root.join(path_str)
            };

            // Canonicalize if possible; fall back to the resolved path on failure
            // (e.g. the file doesn't exist yet — the I/O error below will be more
            // informative).
            let canonical = resolved.canonicalize().unwrap_or(resolved);

            // Lock anchor state and read + anchor the file.
            let mut anchor_state = ctx.anchor_state.lock().expect("anchor state lock poisoned");

            match anchor_state.get_anchors(&canonical) {
                Ok(anchors) => {
                    let output: String = anchors
                        .iter()
                        .map(|(anchor, line)| format!("{anchor}│{line}\n"))
                        .collect();
                    Ok(ToolResult::ok(output))
                }
                Err(e) => Ok(ToolResult::error(format!("read_file failed: {e}"))),
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
    use crate::hashing::state::AnchorState;
    use std::sync::{Arc, Mutex};

    /// Create (or re-use) a temporary directory inside the OS temp dir.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    /// Write `content` to a file at `dir / file_name`.
    fn write_temp_file(dir: &Path, file_name: &str, content: &str) {
        let path = dir.join(file_name);
        std::fs::write(&path, content).expect("failed to write temp file");
    }

    /// Build a minimal `ToolContext` pointing to the given workspace root.
    fn test_context(workspace_root: PathBuf) -> ToolContext {
        ToolContext {
            workspace_root,
            anchor_state: Arc::new(Mutex::new(AnchorState::new())),
        }
    }

    #[tokio::test]
    async fn read_existing_file() {
        let dir = temp_dir("carv-test-read-existing");
        write_temp_file(
            &dir,
            "hello.txt",
            "Hello, World!\nSecond line\nThird line\n",
        );

        let tool = ReadFileTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"path": "hello.txt"}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        // Each line should appear with the │ separator.
        assert!(
            result.content.contains("│Hello, World!"),
            "missing Hello content in:\n{}",
            result.content
        );
        assert!(
            result.content.contains("│Second line"),
            "missing Second line in:\n{}",
            result.content
        );
        assert!(
            result.content.contains("│Third line"),
            "missing Third line in:\n{}",
            result.content
        );
        // Every line must have a non-empty anchor before the │.
        for line in result.content.lines() {
            assert!(line.contains('│'), "line missing │ separator: {line:?}");
            let before = line.split('│').next().unwrap_or("");
            assert!(
                !before.is_empty(),
                "anchor should not be empty in line: {line:?}"
            );
        }
    }

    #[tokio::test]
    async fn file_not_found() {
        let dir = temp_dir("carv-test-not-found");
        let tool = ReadFileTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"path": "nonexistent.txt"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "expected error for nonexistent file");
        assert!(
            result.content.contains("read_file failed"),
            "error message should mention read_file failed, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn path_resolution() {
        let dir = temp_dir("carv-test-path-resolution");

        // Write a file in the workspace root.
        write_temp_file(&dir, "test.txt", "relative content\n");

        let tool = ReadFileTool;
        let ctx = test_context(dir.clone());

        // Relative path — should resolve against workspace_root.
        let result = tool
            .execute(serde_json::json!({"path": "test.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "relative path should succeed");
        assert!(result.content.contains("relative content"));

        // Absolute path — should work regardless of workspace_root.
        let abs_path = dir.join("test.txt");
        let result2 = tool
            .execute(
                serde_json::json!({"path": abs_path.to_str().unwrap()}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result2.is_error, "absolute path should also work");
        assert!(result2.content.contains("relative content"));
    }

    #[tokio::test]
    async fn empty_file() {
        let dir = temp_dir("carv-test-empty-file");
        write_temp_file(&dir, "empty.txt", "");

        let tool = ReadFileTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"path": "empty.txt"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "empty file should succeed");
        assert!(
            result.content.is_empty(),
            "empty file should produce empty output, got: {:?}",
            result.content
        );
    }

    #[tokio::test]
    async fn missing_path_parameter() {
        let dir = temp_dir("carv-test-missing-param");
        let tool = ReadFileTool;
        let ctx = test_context(dir);
        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap();

        assert!(result.is_error, "expected error for missing path");
        assert!(
            result.content.contains("missing required 'path'"),
            "error should mention missing path, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn path_is_not_a_string() {
        let dir = temp_dir("carv-test-not-string");
        let tool = ReadFileTool;
        let ctx = test_context(dir);

        // `path` is a number, not a string.
        let result = tool
            .execute(serde_json::json!({"path": 42}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "expected error for non-string path");
        assert!(
            result.content.contains("missing required 'path'"),
            "error should mention missing path, got: {}",
            result.content
        );
    }
}
