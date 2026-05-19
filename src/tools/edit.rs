//! edit_file tool — anchor-based file editing.
//!
//! Uses stable word anchors (from [`read_file`]) to target edit locations.
//! Supports three operations: `replace` (the default), `insert_before`,
//! and `insert_after`. Multi-file batching is implemented in a subsequent PR.

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Operation enum
// ---------------------------------------------------------------------------

/// Operation dispatched by [`EditFileTool`].
///
/// Parsed once from the `operation` field in the tool input; all downstream
/// matches are exhaustive, so the compiler catches a missing arm whenever a
/// new variant is added.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum EditOp {
    Replace,
    InsertBefore,
    InsertAfter,
}

impl EditOp {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "replace" => Some(Self::Replace),
            "insert_before" => Some(Self::InsertBefore),
            "insert_after" => Some(Self::InsertAfter),
            _ => None,
        }
    }
}

/// Tool for anchor-based file editing.
///
/// Supports three operations: `replace` (default), `insert_before`, and
/// `insert_after`. Uses stable word anchors from
/// [`read_file`](crate::tools::fs::ReadFileTool) to target edit locations.
pub struct EditFileTool;

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file using stable anchor-based line references.\n\
         Supports three operations:\n\
         - replace (default): replace lines from anchor to end_anchor (inclusive)\n\
         - insert_before: insert new lines before the anchor line\n\
         - insert_after: insert new lines after the anchor line\n\
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
                "operation": {
                    "type": "string",
                    "enum": ["replace", "insert_before", "insert_after"],
                    "description": "Edit operation: replace (default), insert_before, or insert_after"
                },
                "anchor": {
                    "type": "string",
                    "description": "Anchor word identifying the target line (from read_file output)"
                },
                "end_anchor": {
                    "type": "string",
                    "description": "Anchor word identifying the last line of the range to replace, inclusive. Required for 'replace' operation."
                },
                "text": {
                    "type": "string",
                    "description": "New text to insert or replace with. Use '\\n' to separate lines. A trailing newline is treated as a line terminator and stripped for all operations. To also produce a trailing blank line in the output, end with '\\n\\n'."
                }
            },
            "required": ["path", "anchor", "text"]
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
            let operation_str = input
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("replace");
            let op = match EditOp::from_str(operation_str) {
                Some(op) => op,
                None => {
                    return Ok(ToolResult::error(format!(
                        "edit_file failed: unknown operation '{}'. \
                         Valid: replace, insert_before, insert_after",
                        operation_str
                    )))
                }
            };
            let anchor = match input.get("anchor").and_then(Value::as_str) {
                Some(a) => a,
                None => return Ok(ToolResult::error("missing required 'anchor' parameter")),
            };
            let end_anchor = if op == EditOp::Replace {
                match input.get("end_anchor").and_then(Value::as_str) {
                    Some(a) => a,
                    None => {
                        return Ok(ToolResult::error(
                            "missing required 'end_anchor' for replace operation",
                        ))
                    }
                }
            } else {
                ""
            };
            let text = match input.get("text").and_then(Value::as_str) {
                Some(t) => t,
                None => return Ok(ToolResult::error("missing required 'text' parameter")),
            };

            // Reject empty text for insert operations — "insert nothing" is
            // almost certainly a caller mistake. (Replace treats empty text as
            // "blank the line", which is a meaningful operation.)
            if matches!(op, EditOp::InsertBefore | EditOp::InsertAfter) && text.is_empty() {
                return Ok(ToolResult::error(
                    "'text' must be non-empty for insert operations",
                ));
            }

            // Resolve path.
            let resolved = if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                ctx.workspace_root.join(path_str)
            };

            // Resolve and validate the path (shared with write_file).
            let resolved =
                match crate::tools::check_path_in_workspace(&resolved, &ctx.workspace_root) {
                    Ok(canon) => canon,
                    Err(msg) => return Ok(ToolResult::error(format!("edit_file failed: {msg}"))),
                };

            // Resolve anchors to line indices. Drop the lock before I/O.
            //
            // NOTE: There is a TOCTOU window between anchor resolution (from
            // cache) and the file read below. The `end_line >= line_count`
            // guard catches file shrinkage, but content changes that preserve
            // line count are not detected. This is inherent to the caching
            // design and acceptable for this scope.
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

                if op == EditOp::Replace {
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
                } else {
                    (start, start) // end_line unused for insert ops
                }
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
            match op {
                EditOp::Replace => {
                    if end_line >= line_count {
                        return Ok(ToolResult::error(format!(
                            "edit_file failed: end_anchor '{}' refers to line {} but \
                             file has {} lines. File may have changed since last read_file.",
                            end_anchor,
                            end_line + 1,
                            line_count
                        )));
                    }
                }
                EditOp::InsertBefore | EditOp::InsertAfter => {
                    if start_line >= line_count {
                        return Ok(ToolResult::error(format!(
                            "edit_file failed: anchor '{}' refers to line {} but \
                             file has {} lines. File may have changed since last read_file.",
                            anchor,
                            start_line + 1,
                            line_count
                        )));
                    }
                }
            }

            // Apply the edit based on operation.
            let new_content = match op {
                EditOp::Replace => replace_line_range(&content, start_line, end_line, text),
                EditOp::InsertBefore => insert_lines_before(&content, start_line, text),
                EditOp::InsertAfter => insert_lines_after(&content, start_line, text),
            };

            // Write back.
            match tokio::fs::write(&resolved, &new_content).await {
                Ok(()) => {
                    let old_bytes = content.len();
                    let new_bytes = new_content.len();

                    // Invalidate anchor cache so the next read_file reflects the edit.
                    {
                        let mut anchor_state =
                            ctx.anchor_state.lock().expect("anchor state lock poisoned");
                        anchor_state.notify_edit(&resolved);
                    }

                    let msg = match op {
                        EditOp::Replace => format!(
                            "Replaced {} line(s) ({}→{} bytes) in {}",
                            end_line - start_line + 1,
                            old_bytes,
                            new_bytes,
                            resolved.display()
                        ),
                        EditOp::InsertBefore | EditOp::InsertAfter => {
                            let dir = if op == EditOp::InsertBefore {
                                "before"
                            } else {
                                "after"
                            };
                            format!(
                                "Inserted {} line(s) {} anchor '{}' ({}→{} bytes) in {}",
                                text.lines().count(),
                                dir,
                                anchor,
                                old_bytes,
                                new_bytes,
                                resolved.display()
                            )
                        }
                    };
                    Ok(ToolResult::ok(msg))
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

/// Insert lines from `new_text` before line `at` (0-indexed) in `content`.
///
/// Splits the file into lines, inserts the new lines at position `at`, and
/// rejoins. Trailing-newline status is preserved. An empty `new_text` is a
/// no-op (returns `content` unchanged).
///
/// Note: `str::lines()` normalizes `\r\n` → `\n`, so Windows-style line
/// endings are converted to LF on write. A trailing `\n` in `new_text` is
/// treated as a line terminator and stripped (consistent with `lines()`
/// semantics). To append a blank line after the last inserted line, end
/// `new_text` with `"\n\n"`.
fn insert_lines_before(content: &str, at: usize, new_text: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    debug_assert!(
        at <= lines.len(),
        "insert_lines_before: at {} out of bounds ({} lines)",
        at,
        lines.len()
    );
    let new_lines: Vec<&str> = new_text.lines().collect();
    if new_lines.is_empty() {
        return content.to_string();
    }
    let has_trailing_newline = content.ends_with('\n');
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + new_lines.len());
    out.extend_from_slice(&lines[..at]);
    out.extend_from_slice(&new_lines);
    out.extend_from_slice(&lines[at..]);
    let mut result = out.join("\n");
    if has_trailing_newline && !result.is_empty() {
        result.push('\n');
    }
    result
}

/// Insert lines from `new_text` after line `at` (0-indexed) in `content`.
///
/// Same semantics as [`insert_lines_before`], but inserts after `at` instead
/// of before. An empty `new_text` is a no-op.
fn insert_lines_after(content: &str, at: usize, new_text: &str) -> String {
    // saturating_add guards against usize::MAX overflow (not reachable
    // in practice — the caller guards at < line_count).
    insert_lines_before(content, at.saturating_add(1), new_text)
}

/// Replace lines from `start_line` to `end_line` (inclusive, 0-indexed) in
/// `content` with `new_text`.
///
/// Splits the file into lines, replaces the specified range, and rejoins.
/// Trailing-newline status is preserved.  The replacement text itself may
/// contain newlines to introduce additional lines.
///
/// Note: `str::lines()` normalizes `\r\n` → `\n`, so Windows-style line
/// endings are converted to LF on write. A trailing `\n` in `new_text` is
/// treated as a line terminator and stripped; `"X\n"` and `"X"` produce
/// the same replacement.
fn replace_line_range(content: &str, start_line: usize, end_line: usize, new_text: &str) -> String {
    let has_trailing_newline = content.ends_with('\n');
    let lines: Vec<&str> = content.lines().collect();
    let mut new_lines: Vec<&str> = new_text.lines().collect();

    // An empty replacement means "make the line empty", not "delete it".
    // `"".lines()` returns zero items (`new_lines.is_empty()` is true iff
    // `new_text` is empty), so push an empty string to preserve the line
    // in the output.
    //
    // NOTE: For single-line files, an empty replacement collapses the file
    // to empty (the trailing-newline logic treats a sole empty line as an
    // empty result). This is an edge case; the guard above ensures line
    // indices are always valid.
    if new_lines.is_empty() {
        new_lines.push("");
    }

    debug_assert!(
        end_line < lines.len(),
        "replace_line_range: end_line {} out of bounds ({} lines)",
        end_line,
        lines.len()
    );

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
    use crate::tools::test_utils::{temp_dir, test_context, write_temp_file};
    use serde_json::json;

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
    fn replace_single_line_with_empty_collapses_file() {
        // Known edge case: sole line + empty replacement → empty file.
        // (Multi-line files preserve the blank line; single-line collapses.)
        assert_eq!(replace_line_range("only\n", 0, 0, ""), "");
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

    #[test]
    fn replace_trailing_newline_in_text_stripped() {
        // str::lines() strips a trailing \n — same as for insert operations.
        let content = "a\nb\nc\n";
        assert_eq!(replace_line_range(content, 1, 1, "X\n"), "a\nX\nc\n");
        assert_eq!(replace_line_range(content, 1, 1, "X"), "a\nX\nc\n");
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
    async fn stale_anchor_after_external_modification() {
        let dir = temp_dir("edit_stale_anchor");
        let file = write_temp_file(&dir, "stale.rs", "line1\nline2\nline3\n");
        let ctx = test_context(dir.clone());

        // Prime the anchor cache.
        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let anchor_line3 = &anchors[2].0; // third line's anchor

        // Simulate external modification: truncate the file to one line.
        // The anchor cache still has 3 lines, but the file now has 1.
        std::fs::write(&file, "only one line\n").unwrap();

        // edit_file should detect the mismatch and refuse to edit.
        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "anchor": anchor_line3,
                    "end_anchor": anchor_line3,
                    "text": "replaced"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("file has"));
        assert!(result.content.contains("File may have changed"));
    }

    #[tokio::test]
    async fn stale_anchor_before_insert() {
        let dir = temp_dir("edit_stale_insert_anchor");
        let file = write_temp_file(&dir, "stale.rs", "line1\nline2\nline3\n");
        let ctx = test_context(dir.clone());

        // Prime the anchor cache.
        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let anchor_line3 = &anchors[2].0; // third line's anchor

        // Truncate the file externally to one line. The anchor cache
        // still reports 3 lines; insert_before should catch the mismatch
        // via its start_line >= line_count guard.
        std::fs::write(&file, "only one line\n").unwrap();

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "insert_before",
                    "anchor": anchor_line3,
                    "text": "inserted"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("file has"));
        assert!(result.content.contains("File may have changed"));
    }

    // -----------------------------------------------------------------------
    // insert_lines_before / insert_lines_after unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn insert_before_first_line() {
        let content = "line1\nline2\n";
        let result = insert_lines_before(content, 0, "header");
        assert_eq!(result, "header\nline1\nline2\n");
    }

    #[test]
    fn insert_before_middle() {
        let content = "a\nb\nc\n";
        let result = insert_lines_before(content, 1, "X\nY");
        assert_eq!(result, "a\nX\nY\nb\nc\n");
    }

    #[test]
    fn insert_after_last_line() {
        let content = "a\nb\n";
        let result = insert_lines_after(content, 1, "c\nd");
        assert_eq!(result, "a\nb\nc\nd\n");
    }

    #[test]
    fn insert_after_middle() {
        let content = "a\nb\nc\n";
        let result = insert_lines_after(content, 0, "X");
        assert_eq!(result, "a\nX\nb\nc\n");
    }

    #[test]
    fn insert_empty_text_is_noop() {
        let content = "a\nb\nc\n";
        assert_eq!(insert_lines_before(content, 1, ""), content);
        assert_eq!(insert_lines_after(content, 1, ""), content);
    }

    #[test]
    fn insert_no_trailing_newline_preserved() {
        let content = "a\nb\nc";
        let result = insert_lines_before(content, 1, "X");
        assert_eq!(result, "a\nX\nb\nc");
    }

    #[test]
    fn insert_crlf_normalized_to_lf() {
        // lines() normalizes \r\n → \n — same behavior as replace_line_range.
        let content = "a\r\nb\r\n";
        let result = insert_lines_before(content, 0, "X");
        assert_eq!(result, "X\na\nb\n");
    }

    #[test]
    fn insert_trailing_newline_in_text_stripped() {
        // str::lines() treats a trailing \n as a terminator, not content.
        // "foo\n" and "foo" produce the same insert. To append a blank
        // line, end with "\n\n".
        let content = "a\nb\n";
        // Both inputs produce the same concrete output — trailing \n is stripped.
        assert_eq!(insert_lines_before(content, 1, "foo\n"), "a\nfoo\nb\n");
        assert_eq!(insert_lines_before(content, 1, "foo"), "a\nfoo\nb\n");
    }

    // -----------------------------------------------------------------------
    // EditFileTool insert_before / insert_after integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn insert_before_anchor() {
        let dir = temp_dir("edit_insert_before");
        let file = write_temp_file(&dir, "target.rs", "// header\nfn main() {\n}\n");
        let ctx = test_context(dir.clone());

        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        // anchors[0] = "// header", anchors[1] = "fn main()"
        let (anchor_fn, _) = &anchors[1];

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "insert_before",
                    "anchor": anchor_fn,
                    "text": "// comment"
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
        assert!(result.content.contains("Inserted 1 line(s) before"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "// header\n// comment\nfn main() {\n}\n"
        );
    }

    #[tokio::test]
    async fn insert_after_anchor() {
        let dir = temp_dir("edit_insert_after");
        let file = write_temp_file(&dir, "target.rs", "let x = 1;\nlet z = 3;\n");
        let ctx = test_context(dir.clone());

        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let (anchor_x, _) = &anchors[0]; // "let x = 1;"

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "insert_after",
                    "anchor": anchor_x,
                    "text": "let y = 2;"
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
        assert!(result.content.contains("Inserted 1 line(s) after"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "let x = 1;\nlet y = 2;\nlet z = 3;\n"
        );
    }

    #[tokio::test]
    async fn insert_before_anchor_cache_invalidated() {
        let dir = temp_dir("edit_insert_cache");
        let file = write_temp_file(&dir, "cache.rs", "original\n");
        let ctx = test_context(dir.clone());

        let anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        let (anchor, _) = &anchors[0];

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "insert_before",
                    "anchor": anchor,
                    "text": "prefix"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);

        // Cache should be invalidated; anchors recomputed from new content.
        let new_anchors = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file).unwrap()
        };
        assert_eq!(new_anchors.len(), 2);
        assert_eq!(new_anchors[0].1, "prefix");
        assert_eq!(new_anchors[1].1, "original");
    }

    #[tokio::test]
    async fn unknown_operation_rejected() {
        let dir = temp_dir("edit_unknown_op");
        let file = write_temp_file(&dir, "f.rs", "a\n");
        let ctx = test_context(dir.clone());

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "delete",
                    "anchor": "any-word",
                    "text": "replacement"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("unknown operation"));
        assert!(result.content.contains("delete"));
    }

    #[tokio::test]
    async fn insert_empty_text_rejected() {
        let dir = temp_dir("edit_insert_empty_text");
        let file = write_temp_file(&dir, "target.rs", "a\nb\n");
        let ctx = test_context(dir.clone());

        // The empty-text guard fires before path resolution or anchor
        // lookup — any anchor string works. We pass a dummy to keep the
        // test focused on parameter validation.
        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "insert_before",
                    "anchor": "any-word",
                    "text": ""
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result
            .content
            .contains("'text' must be non-empty for insert operations"));
    }

    #[tokio::test]
    async fn insert_after_empty_text_rejected() {
        let dir = temp_dir("edit_insert_after_empty");
        let file = write_temp_file(&dir, "target.rs", "a\nb\n");
        let ctx = test_context(dir.clone());

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "operation": "insert_after",
                    "anchor": "any-word",
                    "text": ""
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result
            .content
            .contains("'text' must be non-empty for insert operations"));
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
