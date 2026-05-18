//! edit_file tool — anchor-based file editing.
//!
//! Uses stable word anchors (from [`read_file`]) to target edit locations.
//! This module implements the `replace` operation; `insert_before`,
//! `insert_after`, and multi-file batching are implemented in subsequent PRs.

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Tool for anchor-based file editing.
///
/// Replaces an inclusive range of lines — identified by anchor words from
/// [`read_file`](crate::tools::fs::ReadFileTool) — with new text.
pub struct EditFileTool;

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file using stable anchor-based line references.\n\
         Replaces lines from anchor to end_anchor (inclusive) with new text.\n\
         Use read_file to obtain anchor words for the lines you want to edit."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit, relative to the project root or absolute within the workspace"
                },
                "anchor": {
                    "type": "string",
                    "description": "Anchor word identifying the first line of the range to replace (from read_file output)"
                },
                "end_anchor": {
                    "type": "string",
                    "description": "Anchor word identifying the last line of the range to replace, inclusive (from read_file output)"
                },
                "text": {
                    "type": "string",
                    "description": "New text to replace the range with. Include newline characters between lines as needed."
                }
            },
            "required": ["path", "anchor", "end_anchor", "text"]
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
            let anchor = match input.get("anchor").and_then(Value::as_str) {
                Some(a) => a,
                None => return Ok(ToolResult::error("missing required 'anchor' parameter")),
            };
            let end_anchor = match input.get("end_anchor").and_then(Value::as_str) {
                Some(a) => a,
                None => return Ok(ToolResult::error("missing required 'end_anchor' parameter")),
            };
            let text = match input.get("text").and_then(Value::as_str) {
                Some(t) => t,
                None => return Ok(ToolResult::error("missing required 'text' parameter")),
            };

            // Resolve path.
            let resolved = if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                ctx.workspace_root.join(path_str)
            };

            // Path traversal guard — same logic as write_file.
            {
                let root_canon = ctx
                    .workspace_root
                    .canonicalize()
                    .unwrap_or_else(|_| ctx.workspace_root.clone());
                let mut probe = resolved.clone();
                let bound = loop {
                    match probe.canonicalize() {
                        Ok(c) => break c,
                        Err(_) => match probe.parent() {
                            Some(parent) => probe = parent.to_path_buf(),
                            None => break resolved.clone(),
                        },
                    }
                };
                if !bound.starts_with(&root_canon) {
                    return Ok(ToolResult::error(
                        "edit_file failed: path escapes workspace root",
                    ));
                }
            }

            // Resolve anchors to line indices. Drop the lock before I/O.
            let (start_line, end_line) = {
                let mut anchor_state = ctx.anchor_state.lock().expect("anchor state lock poisoned");
                let anchors = match anchor_state.get_anchors(&resolved) {
                    Ok(a) => a,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "edit_file failed to read anchors: {e}"
                        )))
                    }
                };

                let start = match anchors.iter().position(|(a, _)| a == anchor) {
                    Some(idx) => idx,
                    None => {
                        return Ok(ToolResult::error(format!(
                            "edit_file failed: anchor '{}' not found in '{}'. \
                             Use read_file to get current anchors.",
                            anchor, path_str
                        )))
                    }
                };
                let end = match anchors.iter().position(|(a, _)| a == end_anchor) {
                    Some(idx) => idx,
                    None => {
                        return Ok(ToolResult::error(format!(
                            "edit_file failed: end_anchor '{}' not found in '{}'. \
                             Use read_file to get current anchors.",
                            end_anchor, path_str
                        )))
                    }
                };

                if end < start {
                    return Ok(ToolResult::error(format!(
                        "edit_file failed: end_anchor '{}' (line {}) comes \
                         before anchor '{}' (line {})",
                        end_anchor,
                        end + 1,
                        anchor,
                        start + 1
                    )));
                }

                (start, end)
            }; // lock released

            // Read file from disk.
            let content = match tokio::fs::read_to_string(&resolved).await {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolResult::error(format!(
                        "edit_file failed to read '{}': {e}",
                        path_str
                    )))
                }
            };

            // Guard: ensure referenced lines exist in the current file content.
            let line_count = content.lines().count();
            if end_line >= line_count {
                return Ok(ToolResult::error(format!(
                    "edit_file failed: end_anchor '{}' refers to line {} but \
                     file has {} lines. File may have changed since last read_file.",
                    end_anchor,
                    end_line + 1,
                    line_count
                )));
            }

            // Apply the replace.
            let new_content = replace_line_range(&content, start_line, end_line, text);

            // Write back.
            match tokio::fs::write(&resolved, &new_content).await {
                Ok(()) => {
                    let canonical = resolved.canonicalize().unwrap_or(resolved);
                    let old_bytes = content.len();
                    let new_bytes = new_content.len();
                    let lines_replaced = end_line - start_line + 1;

                    // Invalidate anchor cache so the next read_file reflects the edit.
                    {
                        let mut anchor_state =
                            ctx.anchor_state.lock().expect("anchor state lock poisoned");
                        anchor_state.notify_edit(&canonical);
                    }

                    Ok(ToolResult::ok(format!(
                        "Replaced {} line(s) ({}→{} bytes) in {}",
                        lines_replaced,
                        old_bytes,
                        new_bytes,
                        canonical.display()
                    )))
                }
                Err(e) => Ok(ToolResult::error(format!(
                    "edit_file failed to write '{}': {e}",
                    path_str
                ))),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Replace lines from `start_line` to `end_line` (inclusive, 0-indexed) in
/// `content` with `new_text`.
///
/// Splits the file into lines, replaces the specified range, and rejoins.
/// Trailing-newline status is preserved.  The replacement text itself may
/// contain newlines to introduce additional lines.
fn replace_line_range(content: &str, start_line: usize, end_line: usize, new_text: &str) -> String {
    let has_trailing_newline = content.ends_with('\n');
    let lines: Vec<&str> = content.lines().collect();
    let mut new_lines: Vec<&str> = new_text.lines().collect();

    // An empty replacement means "make the line empty", not "delete it".
    // `"".lines()` returns zero items, so push an empty string to preserve
    // the line in the output.
    if new_lines.is_empty() && new_text.is_empty() {
        new_lines.push("");
    }

    let mut out: Vec<&str> =
        Vec::with_capacity(lines.len() + new_lines.len().saturating_sub(end_line - start_line + 1));
    out.extend_from_slice(&lines[..start_line]);
    out.extend_from_slice(&new_lines);
    out.extend_from_slice(&lines[end_line + 1..]);

    let mut result = out.join("\n");
    if has_trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashing::state::AnchorState;
    use serde_json::json;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("carv-test").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_temp_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn test_context(workspace_root: PathBuf) -> ToolContext {
        ToolContext {
            workspace_root,
            anchor_state: Arc::new(Mutex::new(AnchorState::new())),
        }
    }

    // -----------------------------------------------------------------------
    // replace_line_range unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn replace_single_line() {
        let content = "line1\nline2\nline3\n";
        let result = replace_line_range(content, 1, 1, "replaced");
        assert_eq!(result, "line1\nreplaced\nline3\n");
    }

    #[test]
    fn replace_multi_line_range() {
        let content = "a\nb\nc\nd\ne\n";
        let result = replace_line_range(content, 1, 3, "X\nY");
        assert_eq!(result, "a\nX\nY\ne\n");
    }

    #[test]
    fn replace_first_line() {
        let content = "first\nsecond\n";
        let result = replace_line_range(content, 0, 0, "new first");
        assert_eq!(result, "new first\nsecond\n");
    }

    #[test]
    fn replace_last_line() {
        let content = "alpha\nomega\n";
        let result = replace_line_range(content, 1, 1, "zeta");
        assert_eq!(result, "alpha\nzeta\n");
    }

    #[test]
    fn replace_entire_file() {
        let content = "only\n";
        let result = replace_line_range(content, 0, 0, "replaced\ncompletely");
        assert_eq!(result, "replaced\ncompletely\n");
    }

    #[test]
    fn replace_with_empty_text() {
        let content = "keep\nremove\nkeep\n";
        let result = replace_line_range(content, 1, 1, "");
        assert_eq!(result, "keep\n\nkeep\n");
    }

    #[test]
    fn no_trailing_newline() {
        let content = "a\nb\nc";
        let result = replace_line_range(content, 1, 1, "X");
        assert_eq!(result, "a\nX\nc");
    }

    #[test]
    fn crlf_normalized_to_lf() {
        // lines() normalizes \r\n → \n. This is acceptable because the
        // tool writes back consistent line endings; the LLM can re-apply
        // CRLF with a subsequent edit if needed.
        let content = "a\r\nb\r\nc\r\n";
        let result = replace_line_range(content, 1, 1, "X");
        assert_eq!(result, "a\nX\nc\n");
    }

    #[test]
    fn insert_more_lines_than_removed() {
        let content = "before\nold\nafter\n";
        let result = replace_line_range(content, 1, 1, "one\ntwo\nthree");
        assert_eq!(result, "before\none\ntwo\nthree\nafter\n");
    }

    // -----------------------------------------------------------------------
    // EditFileTool integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn replace_range_in_file() {
        let dir = temp_dir("edit_replace_range");
        let file = write_temp_file(
            &dir,
            "target.rs",
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        );
        let ctx = test_context(dir.clone());

        // Prime the anchor cache via read_file so we can look up anchor words.
        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let (anchor_x, _) = &anchors[1]; // "let x = 1;"
        let (anchor_y, _) = &anchors[2]; // "let y = 2;"

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "anchor": anchor_x,
                    "end_anchor": anchor_y,
                    "text": "    let z = 3;"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("Replaced 2 line(s)"));
        assert!(result.content.contains("target.rs"));

        let new_content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(new_content, "fn main() {\n    let z = 3;\n}\n");
    }

    #[tokio::test]
    async fn anchor_not_found() {
        let dir = temp_dir("edit_anchor_not_found");
        let file = write_temp_file(&dir, "data.rs", "a\nb\nc\n");
        let ctx = test_context(dir.clone());

        // Don't prime the anchor cache — get_anchors will read the file
        // but won't have the nonexistent anchor.
        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "anchor": "nonexistent-anchor-word",
                    "end_anchor": "also-nonexistent",
                    "text": "replacement"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result
            .content
            .contains("anchor 'nonexistent-anchor-word' not found"));
    }

    #[tokio::test]
    async fn end_anchor_before_start_anchor() {
        let dir = temp_dir("edit_anchor_order");
        let file = write_temp_file(&dir, "order.rs", "a\nb\nc\nd\n");
        let ctx = test_context(dir.clone());

        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let anchor_c = &anchors[2].0; // third line
        let anchor_b = &anchors[1].0; // second line (comes before c)

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "anchor": anchor_c,
                    "end_anchor": anchor_b,
                    "text": "replacement"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("comes before anchor"));
    }

    #[tokio::test]
    async fn path_escapes_workspace() {
        let dir = temp_dir("edit_path_escape");
        let ctx = test_context(dir);

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": "../outside.txt",
                    "anchor": "any-anchor",
                    "end_anchor": "any-anchor",
                    "text": "nope"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("escapes workspace root"));
    }

    #[tokio::test]
    async fn file_not_found() {
        let dir = temp_dir("edit_file_not_found");
        let ctx = test_context(dir);

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": "/tmp/carv-nonexistent-edit-target",
                    "anchor": "any-anchor",
                    "end_anchor": "any-anchor",
                    "text": "nope"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        // Could be an anchor read failure or a file read failure — either is fine.
        assert!(
            result.content.contains("edit_file failed"),
            "expected error, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn anchor_cache_invalidated_after_edit() {
        let dir = temp_dir("edit_cache_invalidation");
        let file = write_temp_file(&dir, "cache.rs", "original\n");
        let ctx = test_context(dir.clone());

        // Prime the cache.
        let initial_anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let original_anchor = &initial_anchors[0].0;
        assert_eq!(initial_anchors[0].1, "original");

        // Replace the line.
        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "anchor": original_anchor,
                    "end_anchor": original_anchor,
                    "text": "modified"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);

        // After edit, anchors should be recomputed (cache invalidated).
        let new_anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        assert_eq!(new_anchors[0].1, "modified");
        // Anchor word for the same line may differ because content changed.
        assert_ne!(new_anchors[0].0, *original_anchor);
    }
}
