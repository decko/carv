//! Filesystem tools — [`ReadFileTool`], [`WriteFileTool`], [`ListFilesTool`].
//!
//! These provide the core filesystem operations the LLM agent uses to
//! read, write, and list files in the project workspace.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};

// ---------------------------------------------------------------------------
// ReadFileTool
// ---------------------------------------------------------------------------

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
// WriteFileTool
// ---------------------------------------------------------------------------

/// Tool that creates or overwrites a file in the project workspace.
///
/// Writes the provided content to the specified path. On success, invalidates
/// the anchor cache for that file so subsequent reads return fresh anchors.
pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file in the project workspace. \
         Creates a new file or overwrites an existing one. \
         Parent directories must already exist."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write, relative to the project root or absolute"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            let path_str = match input.get("path").and_then(Value::as_str) {
                Some(p) => p,
                None => return Ok(ToolResult::error("missing required 'path' parameter")),
            };

            let content = match input.get("content").and_then(Value::as_str) {
                Some(c) => c,
                None => return Ok(ToolResult::error("missing required 'content' parameter")),
            };

            let resolved = if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                ctx.workspace_root.join(path_str)
            };

            match tokio::fs::write(&resolved, content).await {
                Ok(()) => {
                    let byte_count = content.len();
                    // Canonicalize AFTER the write so the key matches what
                    // ReadFileTool will resolve.  If the file is new, a
                    // pre-write canonicalize would fall back to the raw
                    // `resolved` path while ReadFileTool would later get
                    // the true canonical path — breaking cache invalidation
                    // when a directory component is a symlink.
                    let canonical = resolved.canonicalize().unwrap_or(resolved);
                    let mut anchor_state =
                        ctx.anchor_state.lock().expect("anchor state lock poisoned");
                    anchor_state.notify_edit(&canonical);
                    Ok(ToolResult::ok(format!(
                        "Wrote {byte_count} bytes to {}",
                        canonical.display()
                    )))
                }
                Err(e) => Ok(ToolResult::error(format!("write_file failed: {e}"))),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// ListFilesTool
// ---------------------------------------------------------------------------

/// Tool that lists directory contents with `.gitignore`-aware filtering.
///
/// Uses the `ignore` crate to walk the directory, respecting `.gitignore`
/// and `.ignore` patterns. Hidden files and directories (dot-prefixed,
/// such as `.env` or `.cargo/`) are also excluded by the default filters.
/// Returns relative paths to files, one per line.
pub struct ListFilesTool;

impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List files in a directory within the project workspace. \
         Results respect .gitignore and .ignore patterns. \
         Hidden files and directories (dot-prefixed) are excluded. \
         Returns relative file paths, one per line."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to list, relative to the project root or absolute (defaults to the project root)"
                },
                "max_entries": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (default: 200)"
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            let path_str = input.get("path").and_then(Value::as_str).unwrap_or(".");

            let max_entries = input
                .get("max_entries")
                .and_then(Value::as_u64)
                .map(|v| usize::try_from(v).unwrap_or(200))
                .unwrap_or(200)
                .min(10_000);

            let resolved = if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                ctx.workspace_root.join(path_str)
            };

            let canonical = resolved.canonicalize().unwrap_or(resolved);

            // Check that the path exists and is a directory before walking.
            if !canonical.is_dir() {
                return Ok(ToolResult::error(format!(
                    "list_files failed: {}",
                    if canonical.exists() {
                        "not a directory"
                    } else {
                        "path does not exist"
                    }
                )));
            }

            let walker = ignore::WalkBuilder::new(&canonical)
                .standard_filters(true)
                .require_git(false)
                .build();

            let mut entries: Vec<String> = Vec::new();

            for result in walker {
                match result {
                    Ok(entry) => {
                        let path = entry.path();
                        // Skip the root directory itself.
                        if path == canonical {
                            continue;
                        }
                        // Only list files, not directories.  The tool name is
                        // "list_files" and returning directory entries would
                        // confuse the LLM into trying to open them.
                        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                            continue;
                        }
                        // Get the relative path from the listing directory.
                        if let Ok(rel) = path.strip_prefix(&canonical) {
                            let rel_str = rel.to_string_lossy().to_string();
                            // Normalize path separators to forward slashes
                            // (on Windows, strip_prefix may produce backslashes).
                            #[cfg(windows)]
                            let rel_str = rel_str.replace('\\', "/");
                            entries.push(rel_str);
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "list_files: error walking entry");
                    }
                }
            }

            entries.sort();

            let truncated = entries.len() > max_entries;
            entries.truncate(max_entries);

            let mut output = entries.join("\n");
            if truncated {
                use std::fmt::Write;
                write!(output, "\n... truncated at {max_entries} entries")
                    .expect("write to String is infallible");
            }

            if output.is_empty() {
                Ok(ToolResult::ok("(empty directory)"))
            } else {
                Ok(ToolResult::ok(output))
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
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use crate::hashing::state::AnchorState;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Create an empty temporary directory and return its path.
    ///
    /// Removes any leftover directory from a previous run first, so tests
    /// start with a clean slate regardless of state on disk.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("carv-test").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `content` to a file at `dir/name` and return the full path.
    fn write_temp_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    /// Create a minimal `ToolContext` for a given workspace root.
    fn test_context(workspace_root: PathBuf) -> ToolContext {
        ToolContext {
            workspace_root,
            anchor_state: Arc::new(Mutex::new(AnchorState::new())),
        }
    }

    // -----------------------------------------------------------------------
    // ReadFileTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_existing_file() {
        let dir = temp_dir("read_existing_file");
        let file = write_temp_file(&dir, "hello.rs", "fn hello() {}\n");
        let ctx = test_context(dir);

        let tool = ReadFileTool;
        let result = tool
            .execute(json!({"path": file.to_str().unwrap()}), &ctx)
            .await
            .unwrap();
        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("│fn hello() {}\n"));
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let dir = temp_dir("read_file_not_found");
        let ctx = test_context(dir);

        let tool = ReadFileTool;
        let result = tool
            .execute(json!({"path": "/tmp/carv-nonexistent-file-12345"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("read_file failed"));
    }

    #[tokio::test]
    async fn read_path_resolution() {
        let dir = temp_dir("read_path_resolution");
        write_temp_file(&dir, "target.rs", "// content\n");
        let ctx = test_context(dir.clone());

        let tool = ReadFileTool;

        // Relative path
        let rel_result = tool
            .execute(json!({"path": "target.rs"}), &ctx)
            .await
            .unwrap();
        assert!(
            !rel_result.is_error,
            "relative path failed: {}",
            rel_result.content
        );

        // Absolute path
        let abs_path = dir.join("target.rs");
        let abs_result = tool
            .execute(json!({"path": abs_path.to_str().unwrap()}), &ctx)
            .await
            .unwrap();
        assert!(
            !abs_result.is_error,
            "absolute path failed: {}",
            abs_result.content
        );

        assert_eq!(
            rel_result.content, abs_result.content,
            "relative and absolute paths should produce identical output"
        );
    }

    #[tokio::test]
    async fn read_empty_file() {
        let dir = temp_dir("read_empty_file");
        let file = write_temp_file(&dir, "empty.rs", "");
        let ctx = test_context(dir);

        let tool = ReadFileTool;
        let result = tool
            .execute(json!({"path": file.to_str().unwrap()}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "");
    }

    #[tokio::test]
    async fn read_missing_path_parameter() {
        let dir = temp_dir("read_missing_path");
        let ctx = test_context(dir);

        let tool = ReadFileTool;
        let result = tool.execute(json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "missing required 'path' parameter");
    }

    #[tokio::test]
    async fn read_path_is_not_a_string() {
        let dir = temp_dir("read_path_not_string");
        let ctx = test_context(dir);

        let tool = ReadFileTool;
        let result = tool.execute(json!({"path": 42}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "missing required 'path' parameter");
    }

    // -----------------------------------------------------------------------
    // WriteFileTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn write_new_file() {
        let dir = temp_dir("write_new_file");
        let ctx = test_context(dir.clone());

        let file_path = dir.join("new_file.rs");

        let tool = WriteFileTool;
        let result = tool
            .execute(
                json!({"path": file_path.to_str().unwrap(), "content": "fn new() {}"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "write failed: {}", result.content);
        assert!(result.content.contains("Wrote"));
        assert!(result.content.contains("bytes"));

        // Verify file content on disk.
        let on_disk = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(on_disk, "fn new() {}");
    }

    #[tokio::test]
    async fn overwrite_existing_file() {
        let dir = temp_dir("overwrite_existing_file");
        let file = write_temp_file(&dir, "existing.rs", "original content");
        let ctx = test_context(dir);

        let tool = WriteFileTool;
        let result = tool
            .execute(
                json!({"path": file.to_str().unwrap(), "content": "modified content"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "overwrite failed: {}", result.content);

        // Verify file content was updated.
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert_eq!(on_disk, "modified content");
    }

    #[tokio::test]
    async fn write_anchor_cache_invalidation() {
        let dir = temp_dir("write_cache_inval");
        let file = write_temp_file(&dir, "cache_test.rs", "original\n");

        // Read via AnchorState to populate the cache.
        let anchor_state = Arc::new(Mutex::new(AnchorState::new()));
        {
            let mut state = anchor_state.lock().unwrap();
            let _ = state.get_anchors(&file).unwrap();
            assert_eq!(
                state.file_count(),
                1,
                "should have 1 cached file after read"
            );
        }

        let ctx = ToolContext {
            workspace_root: dir,
            anchor_state: anchor_state.clone(),
        };

        // Write via the tool — this should invalidate the cache.
        let tool = WriteFileTool;
        let result = tool
            .execute(
                json!({"path": file.to_str().unwrap(), "content": "modified\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "write failed: {}", result.content);

        // Verify cache was invalidated.
        let state = anchor_state.lock().unwrap();
        assert_eq!(
            state.file_count(),
            0,
            "anchor cache should be invalidated after write"
        );
    }

    #[tokio::test]
    async fn write_missing_parameters() {
        let dir = temp_dir("write_missing_params");
        let ctx = test_context(dir);

        let tool = WriteFileTool;

        // Missing both path and content.
        let result = tool.execute(json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "missing required 'path' parameter");

        // Missing content.
        let result = tool
            .execute(json!({"path": "some_file.rs"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "missing required 'content' parameter");
    }

    #[tokio::test]
    async fn write_fails_when_parent_missing() {
        let dir = temp_dir("write_to_subdir");
        let ctx = test_context(dir);

        let tool = WriteFileTool;

        // Try to write to a subdirectory that does NOT exist.
        let result = tool
            .execute(
                json!({"path": "nonexistent_subdir/new_file.rs", "content": "// test"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error, "should fail when parent dir doesn't exist");
        assert!(result.content.contains("write_file failed"));
    }

    // -----------------------------------------------------------------------
    // ListFilesTool tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_directory_contents() {
        let dir = temp_dir("list_contents");
        write_temp_file(&dir, "alpha.rs", "// alpha");
        write_temp_file(&dir, "beta.rs", "// beta");
        // Create a subdirectory with a file.
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        write_temp_file(&sub, "gamma.rs", "// gamma");

        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool.execute(json!({"path": "."}), &ctx).await.unwrap();

        assert!(!result.is_error, "list failed: {}", result.content);

        // Output should contain all files (and subdirectory).
        assert!(result.content.contains("alpha.rs"), "missing alpha.rs");
        assert!(result.content.contains("beta.rs"), "missing beta.rs");
        assert!(result.content.contains("gamma.rs"), "missing gamma.rs");
        // Paths should be relative.
        assert!(
            result.content.contains("sub/gamma.rs"),
            "missing sub/gamma.rs (got: {})",
            result.content
        );
    }

    #[tokio::test]
    async fn list_max_entries_truncation() {
        let dir = temp_dir("list_truncation");
        // Create more files than the max_entries limit.
        for i in 0..50 {
            write_temp_file(&dir, &format!("file_{i}.rs"), "// placeholder");
        }

        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool
            .execute(json!({"path": ".", "max_entries": 10}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "list failed: {}", result.content);

        // 10 entries + 1 truncation line = 11 lines.
        assert!(
            result.content.contains("truncated at 10 entries"),
            "should contain truncation message"
        );
        // There should be exactly 10 file entries + the truncation note.
        // Count everything except the truncation line as entries.
        let entry_lines: Vec<&str> = result
            .content
            .lines()
            .filter(|l| !l.starts_with("... truncated"))
            .collect();
        assert_eq!(entry_lines.len(), 10, "expected 10 entries");
    }

    #[tokio::test]
    async fn list_empty_directory() {
        let dir = temp_dir("list_empty");
        // Empty temp directory.
        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool.execute(json!({"path": "."}), &ctx).await.unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "(empty directory)");
    }

    #[tokio::test]
    async fn list_respects_ignore_filters() {
        let dir = temp_dir("list_ignore_filters");

        // Create a .gitignore that ignores .log files.
        write_temp_file(&dir, ".gitignore", "*.log\n");

        // Create some tracked and ignored files.
        write_temp_file(&dir, "keep.rs", "// this stays");
        write_temp_file(&dir, "ignore.log", "this should be ignored");
        write_temp_file(&dir, "also_keep.py", "print('hi')");

        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool.execute(json!({"path": "."}), &ctx).await.unwrap();

        assert!(!result.is_error, "list failed: {}", result.content);

        assert!(result.content.contains("keep.rs"), "should contain keep.rs");
        assert!(
            result.content.contains("also_keep.py"),
            "should contain also_keep.py"
        );
        assert!(
            !result.content.contains("ignore.log"),
            "should NOT contain ignore.log"
        );
        // `standard_filters(true)` enables the `hidden` filter, which hides
        // dot-files including `.gitignore` itself.  Assert it is absent so
        // tests don't accidentally depend on filter internals that may change.
        assert!(
            !result.content.contains(".gitignore"),
            "standard_filters hides dot-files including .gitignore"
        );
    }

    #[tokio::test]
    async fn list_default_path() {
        let dir = temp_dir("list_default_path");
        write_temp_file(&dir, "default_test.rs", "// default path test");

        // The workspace_root points to our temp dir; omitting "path" defaults to ".".
        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool.execute(json!({}), &ctx).await.unwrap();

        assert!(
            !result.is_error,
            "list with default path failed: {}",
            result.content
        );
        assert!(
            result.content.contains("default_test.rs"),
            "should find file with default path"
        );
    }

    #[tokio::test]
    async fn list_nonexistent_directory_errors() {
        let dir = temp_dir("list_nonexistent_dir");
        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool
            .execute(json!({"path": "nonexistent_dir"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "expected error for nonexistent directory");
        assert!(
            result.content.contains("list_files failed"),
            "error should mention list_files failed, got: {}",
            result.content
        );
        assert!(
            result.content.contains("path does not exist"),
            "error should state the path does not exist, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn list_file_not_directory_errors() {
        let dir = temp_dir("list_file_not_dir");
        write_temp_file(&dir, "a_file.rs", "// not a directory");

        let ctx = test_context(dir);

        let tool = ListFilesTool;
        let result = tool
            .execute(json!({"path": "a_file.rs"}), &ctx)
            .await
            .unwrap();

        assert!(
            result.is_error,
            "expected error when listing a file, got success: {}",
            result.content
        );
        assert!(
            result.content.contains("not a directory"),
            "error should state 'not a directory', got: {}",
            result.content
        );
    }
}
