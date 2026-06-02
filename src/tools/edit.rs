//! edit_file tool — anchor-based file editing.
//!
//! Uses stable word anchors (from [`read_file`]) to target edit locations.
//! Supports three operations: `replace` (the default), `insert_before`,
//! and `insert_after`. Edits are batched per file via `check_overlaps`; edits
//! within a file are applied bottom-to-top (sorted by `start` descending)
//! to preserve line-index validity.

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

/// A fully-resolved edit operation ready for application.
struct ResolvedEdit {
    operation: EditOp,
    /// 0-indexed line number.
    start: usize,
    /// 0-indexed line number. For inserts: `end == start`.
    end: usize,
    /// The replacement/insertion text.
    text: String,
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
        "Edit one or more files using stable anchor-based line references.\n\
         Provide a 'files' array where each entry has a 'path' and an 'edits'\n\
         array. Each edit supports three operations:\n\
         - replace (default): replace lines from anchor to end_anchor (inclusive)\n\
         - insert_before: insert new lines before the anchor line\n\
         - insert_after: insert new lines after the anchor line\n\
         Use read_file to obtain anchor words for the lines you want to edit.\n\
         Overlapping edits within the same file are rejected."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "description": "Array of file edit entries. Each entry specifies a path and the edits to apply to that file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Path to the file to edit, relative to the project root or absolute within the workspace"
                            },
                            "edits": {
                                "type": "array",
                                "description": "Array of edit operations to apply to this file",
                                "items": {
                                    "type": "object",
                                    "properties": {
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
                                    "required": ["anchor", "text"]
                                }
                            }
                        },
                        "required": ["path", "edits"]
                    }
                }
            },
            "required": ["files"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            let files_array = match input.get("files").and_then(Value::as_array) {
                Some(arr) if !arr.is_empty() => arr,
                Some(_) => return Ok(ToolResult::error("'files' array must not be empty")),
                None => return Ok(ToolResult::error("missing required 'files' parameter")),
            };

            let mut results: Vec<String> = Vec::new();
            let mut any_success = false;

            for (file_idx, file_entry) in files_array.iter().enumerate() {
                let path_str = match file_entry.get("path").and_then(Value::as_str) {
                    Some(p) => p,
                    None => {
                        results.push(format!("edit_file: missing 'path' in files[{}]", file_idx));
                        continue;
                    }
                };

                let edits_array = match file_entry.get("edits").and_then(Value::as_array) {
                    Some(arr) if !arr.is_empty() => arr,
                    Some(_) => {
                        results.push(format!(
                            "edit_file: 'edits' array is empty in '{}'",
                            path_str
                        ));
                        continue;
                    }
                    None => {
                        results.push(format!(
                            "edit_file: missing 'edits' array in '{}'",
                            path_str
                        ));
                        continue;
                    }
                };

                // --- validate all edits for this file ---
                let mut parsed_edits: Vec<(EditOp, &str, &str, &str)> = Vec::new();
                // (op, anchor, end_anchor, text)
                let mut edit_error: Option<String> = None;

                for (edit_idx, edit_entry) in edits_array.iter().enumerate() {
                    let operation_str = edit_entry
                        .get("operation")
                        .and_then(Value::as_str)
                        .unwrap_or("replace");
                    let op = match EditOp::from_str(operation_str) {
                        Some(op) => op,
                        None => {
                            edit_error = Some(format!(
                                "edit_file: unknown operation '{}' in '{}' edit {}. \
                                 Valid: replace, insert_before, insert_after",
                                operation_str, path_str, edit_idx
                            ));
                            break;
                        }
                    };

                    let anchor = match edit_entry.get("anchor").and_then(Value::as_str) {
                        Some(a) => a,
                        None => {
                            edit_error = Some(format!(
                                "edit_file: missing 'anchor' in '{}' edit {}",
                                path_str, edit_idx
                            ));
                            break;
                        }
                    };

                    let end_anchor = if op == EditOp::Replace {
                        match edit_entry.get("end_anchor").and_then(Value::as_str) {
                            Some(a) => a,
                            None => {
                                edit_error = Some(format!(
                                    "edit_file: missing 'end_anchor' for replace in '{}' edit {}",
                                    path_str, edit_idx
                                ));
                                break;
                            }
                        }
                    } else {
                        ""
                    };

                    let text = match edit_entry.get("text").and_then(Value::as_str) {
                        // Replace allows empty text ("blank the line"); inserts require content.
                        Some(t) if !t.is_empty() || op == EditOp::Replace => t,
                        Some(_) => {
                            edit_error = Some(format!(
                                "edit_file: 'text' must be non-empty for insert in '{}' edit {}",
                                path_str, edit_idx
                            ));
                            break;
                        }
                        None => {
                            edit_error = Some(format!(
                                "edit_file: missing 'text' in '{}' edit {}",
                                path_str, edit_idx
                            ));
                            break;
                        }
                    };

                    parsed_edits.push((op, anchor, end_anchor, text));
                }

                if let Some(err) = edit_error {
                    results.push(err);
                    continue;
                }

                // --- resolve path ---
                let resolved_raw = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    ctx.workspace_root.join(path_str)
                };

                let resolved =
                    match crate::tools::check_path_in_workspace(&resolved_raw, &ctx.workspace_root)
                    {
                        Ok(canon) => canon,
                        Err(msg) => {
                            results.push(format!("edit_file: {msg}"));
                            continue;
                        }
                    };

                // --- resolve anchors and build ResolvedEdit list ---
                //
                // NOTE: There is a TOCTOU window between anchor resolution
                // (from cache) and the file read below. The line-count guard
                // catches file shrinkage, but content changes that preserve
                // line count are not detected. This is inherent to the caching
                // design and acceptable for this scope.
                let mut edits: Vec<ResolvedEdit> = {
                    let mut anchor_state = match ctx.anchor_state.lock() {
                        Ok(guard) => guard,
                        Err(_) => {
                            results.push(format!(
                                "edit_file: anchor state lock poisoned for '{}'",
                                path_str
                            ));
                            continue;
                        }
                    };
                    let anchors = match anchor_state.get_anchors(&resolved) {
                        Ok(a) => a,
                        Err(e) => {
                            results.push(format!(
                                "edit_file: failed to read anchors for '{}': {e}",
                                path_str
                            ));
                            continue;
                        }
                    };

                    let mut edits = Vec::with_capacity(parsed_edits.len());
                    let mut anchor_error: Option<String> = None;

                    for (op, anchor, end_anchor, text) in &parsed_edits {
                        let start = match anchors.iter().position(|(a, _)| a == *anchor) {
                            Some(idx) => idx,
                            None => {
                                anchor_error = Some(format!(
                                    "edit_file: anchor '{}' not found in '{}'. \
                                     Use read_file to get current anchors.",
                                    anchor, path_str
                                ));
                                break;
                            }
                        };

                        if *op == EditOp::Replace {
                            let end = match anchors.iter().position(|(a, _)| a == *end_anchor) {
                                Some(idx) => idx,
                                None => {
                                    anchor_error = Some(format!(
                                        "edit_file: end_anchor '{}' not found in '{}'. \
                                             Use read_file to get current anchors.",
                                        end_anchor, path_str
                                    ));
                                    break;
                                }
                            };

                            if end < start {
                                anchor_error = Some(format!(
                                    "edit_file: end_anchor '{}' (line {}) comes before \
                                     anchor '{}' (line {}) in '{}'",
                                    end_anchor,
                                    end + 1,
                                    anchor,
                                    start + 1,
                                    path_str
                                ));
                                break;
                            }

                            edits.push(ResolvedEdit {
                                operation: *op,
                                start,
                                end,
                                text: (*text).to_string(),
                            });
                        } else {
                            edits.push(ResolvedEdit {
                                operation: *op,
                                start,
                                end: start,
                                text: (*text).to_string(),
                            });
                        }
                    }

                    if let Some(err) = anchor_error {
                        results.push(err);
                        continue;
                    }

                    edits
                }; // lock released

                // --- check for overlapping edits ---
                if let Err(err) = check_overlaps(&edits, &resolved.display().to_string()) {
                    results.push(err.content);
                    continue;
                }

                // --- read file from disk ---
                let content = match tokio::fs::read_to_string(&resolved).await {
                    Ok(c) => c,
                    Err(e) => {
                        results.push(format!("edit_file: failed to read '{}': {e}", path_str));
                        continue;
                    }
                };

                // --- guard: line-count checks ---
                let line_count = content.lines().count();
                let mut guard_error: Option<String> = None;
                for edit in &edits {
                    match edit.operation {
                        EditOp::Replace => {
                            if edit.end >= line_count {
                                guard_error = Some(format!(
                                    "edit_file: line range {}-{} out of bounds in '{}' \
                                     (file has {} lines). File may have changed.",
                                    edit.start + 1,
                                    edit.end + 1,
                                    path_str,
                                    line_count
                                ));
                                break;
                            }
                        }
                        EditOp::InsertBefore | EditOp::InsertAfter => {
                            if edit.start >= line_count {
                                guard_error = Some(format!(
                                    "edit_file: target line {} out of bounds in '{}' \
                                     (file has {} lines). File may have changed.",
                                    edit.start + 1,
                                    path_str,
                                    line_count
                                ));
                                break;
                            }
                        }
                    }
                }
                if let Some(err) = guard_error {
                    results.push(err);
                    continue;
                }

                // --- apply edits bottom-to-top (preserves line indices) ---
                let old_bytes = content.len();
                let mut new_content = content;
                // Sort descending by start: higher lines applied first so
                // lower-line edits' positions remain valid after insertions
                // or multi-line replacements above them.
                edits.sort_by_key(|e| std::cmp::Reverse(e.start));
                for edit in &edits {
                    new_content = match edit.operation {
                        EditOp::Replace => {
                            replace_line_range(&new_content, edit.start, edit.end, &edit.text)
                        }
                        EditOp::InsertBefore => {
                            insert_lines_before(&new_content, edit.start, &edit.text)
                        }
                        EditOp::InsertAfter => {
                            insert_lines_after(&new_content, edit.start, &edit.text)
                        }
                    };
                }

                // --- write back ---
                match tokio::fs::write(&resolved, &new_content).await {
                    Ok(()) => {
                        let new_bytes = new_content.len();

                        // Invalidate anchor cache.
                        {
                            match ctx.anchor_state.lock() {
                                Ok(mut anchor_state) => {
                                    anchor_state.notify_edit(&resolved);
                                }
                                Err(_) => {
                                    // Cache invalidation failure is non-fatal;
                                    // the file was already written successfully.
                                }
                            }
                            // Also invalidate the tree-sitter parser cache.
                            if let Ok(mut pc) = ctx.parser_cache.lock() {
                                pc.invalidate(&resolved);
                            }
                        }

                        results.push(format!(
                            "Applied {} edit(s) ({}→{} bytes) to {}",
                            edits.len(),
                            old_bytes,
                            new_bytes,
                            path_str
                        ));
                        any_success = true;
                    }
                    Err(e) => {
                        results.push(format!("edit_file: failed to write '{}': {e}", path_str));
                    }
                }
            }

            // --- assemble final result ---
            if any_success {
                Ok(ToolResult::ok(results.join("\n")))
            } else {
                Ok(ToolResult::error(results.join("\n")))
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

/// Check a list of edits for overlapping line ranges.
///
/// Two edits overlap if their line ranges intersect:
/// `a.start <= b.end && b.start <= a.end`.
///
/// Adjacent inserts (e.g., insert_after at line 1 and insert_before at line 2)
/// are NOT overlapping — their ranges `[1,1]` and `[2,2]` are disjoint.
///
/// Two point-edit operations targeting the same line (e.g., insert_after +
/// insert_before around line N) are NOT treated as overlapping — the
/// `(InsertAfter, InsertBefore)` pair on the same line is a valid "wrap" pattern.
///
/// # Errors
///
/// Returns `Err(ToolResult::error(...))` with a message describing the first
/// overlap found, or `Ok(())` if no overlaps are detected.
fn check_overlaps(edits: &[ResolvedEdit], path: &str) -> Result<(), ToolResult> {
    for i in 0..edits.len() {
        for j in (i + 1)..edits.len() {
            let a = &edits[i];
            let b = &edits[j];
            // Two edits overlap if their line ranges intersect.
            if a.start <= b.end && b.start <= a.end {
                // Exception: insert_after + insert_before at the same line is
                // a valid "wrap" pattern (both are point ops at [N,N]).
                let is_same_line_point_ops =
                    a.end == a.start && b.end == b.start && a.start == b.start;
                let is_wrap_pair = matches!(
                    (a.operation, b.operation),
                    (EditOp::InsertAfter, EditOp::InsertBefore)
                        | (EditOp::InsertBefore, EditOp::InsertAfter)
                );
                if is_same_line_point_ops && is_wrap_pair {
                    continue;
                }
                return Err(ToolResult::error(format!(
                    "Overlapping edit ranges in '{}': edit {} (lines {}-{}) \
                     overlaps with edit {} (lines {}-{})",
                    path,
                    i,
                    a.start + 1,
                    a.end + 1,
                    j,
                    b.start + 1,
                    b.end + 1,
                )));
            }
        }
    }
    Ok(())
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

    #[test]
    fn replace_double_newline_appends_blank_line() {
        // "\n\n" in new_text keeps content + appends a trailing blank line.
        let content = "a\nb\nc\n";
        assert_eq!(replace_line_range(content, 1, 1, "X\n\n"), "a\nX\n\nc\n");
    }

    // -----------------------------------------------------------------------
    // EditFileTool integration tests
    // -----------------------------------------------------------------------

    /// Build a `files`-array JSON payload from a single file with one edit.
    fn single_file_edit(path: &str, edit: Value) -> Value {
        json!({
            "files": [{
                "path": path,
                "edits": [edit]
            }]
        })
    }

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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "anchor": anchor_x,
                        "end_anchor": anchor_y,
                        "text": "    let z = 3;"
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("Applied 1 edit(s)"));
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "anchor": "nonexistent-anchor-word",
                        "end_anchor": "also-nonexistent",
                        "text": "replacement"
                    }),
                ),
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "anchor": anchor_c,
                        "end_anchor": anchor_b,
                        "text": "replacement"
                    }),
                ),
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
                single_file_edit(
                    "../outside.txt",
                    json!({
                        "anchor": "any-anchor",
                        "end_anchor": "any-anchor",
                        "text": "nope"
                    }),
                ),
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
                single_file_edit(
                    "/tmp/carv-nonexistent-edit-target",
                    json!({
                        "anchor": "any-anchor",
                        "end_anchor": "any-anchor",
                        "text": "nope"
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        // Could be an anchor read failure, a file read failure, or a
        // workspace-escape rejection — any edit_file error is fine.
        assert!(
            result.content.contains("edit_file:"),
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "anchor": anchor_line3,
                        "end_anchor": anchor_line3,
                        "text": "replaced"
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("out of bounds"));
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "insert_before",
                        "anchor": anchor_line3,
                        "text": "inserted"
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("out of bounds"));
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

    #[test]
    fn insert_double_newline_appends_blank_line() {
        // "\n\n" keeps one line of content + appends a trailing blank line.
        let content = "a\nb\n";
        assert_eq!(insert_lines_before(content, 1, "foo\n\n"), "a\nfoo\n\nb\n");
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "insert_before",
                        "anchor": anchor_fn,
                        "text": "// comment"
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("Applied 1 edit(s)"));
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "insert_after",
                        "anchor": anchor_x,
                        "text": "let y = 2;"
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(result.content.contains("Applied 1 edit(s)"));
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "insert_before",
                        "anchor": anchor,
                        "text": "prefix"
                    }),
                ),
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "delete",
                        "anchor": "any-word",
                        "text": "replacement"
                    }),
                ),
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "insert_before",
                        "anchor": "any-word",
                        "text": ""
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result
            .content
            .contains("'text' must be non-empty for insert"));
    }

    #[tokio::test]
    async fn insert_after_empty_text_rejected() {
        let dir = temp_dir("edit_insert_after_empty");
        let file = write_temp_file(&dir, "target.rs", "a\nb\n");
        let ctx = test_context(dir.clone());

        let tool = EditFileTool;
        let result = tool
            .execute(
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "operation": "insert_after",
                        "anchor": "any-word",
                        "text": ""
                    }),
                ),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result
            .content
            .contains("'text' must be non-empty for insert"));
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
                single_file_edit(
                    file.to_str().unwrap(),
                    json!({
                        "anchor": original_anchor,
                        "end_anchor": original_anchor,
                        "text": "modified"
                    }),
                ),
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

    // -----------------------------------------------------------------------
    // Multi-file integration test
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn multi_file_edit() {
        let dir = temp_dir("edit_multi_file");
        let file_a = write_temp_file(&dir, "a.rs", "pub fn a() {}\n");
        let file_b = write_temp_file(&dir, "b.rs", "pub fn b() {}\n");
        let ctx = test_context(dir.clone());

        // Prime anchor caches for both files.
        let anchors_a = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file_a).unwrap()
        };
        let anchors_b = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file_b).unwrap()
        };
        let (anchor_a, _) = &anchors_a[0];
        let (anchor_b, _) = &anchors_b[0];

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "files": [
                        {
                            "path": file_a.to_str().unwrap(),
                            "edits": [
                                {
                                    "anchor": anchor_a,
                                    "end_anchor": anchor_a,
                                    "text": "pub fn a() { /* new */ }"
                                }
                            ]
                        },
                        {
                            "path": file_b.to_str().unwrap(),
                            "edits": [
                                {
                                    "anchor": anchor_b,
                                    "end_anchor": anchor_b,
                                    "text": "pub fn b() { /* new */ }"
                                }
                            ]
                        }
                    ]
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
        assert!(result.content.contains("Applied 1 edit(s)"));
        assert!(result.content.contains("a.rs"));
        assert!(result.content.contains("b.rs"));

        assert_eq!(
            std::fs::read_to_string(&file_a).unwrap(),
            "pub fn a() { /* new */ }\n"
        );
        assert_eq!(
            std::fs::read_to_string(&file_b).unwrap(),
            "pub fn b() { /* new */ }\n"
        );
    }

    #[tokio::test]
    async fn multi_file_partial_success() {
        let dir = temp_dir("edit_partial_success");
        let file_ok = write_temp_file(&dir, "ok.rs", "fn ok() {}\n");
        let ctx = test_context(dir.clone());

        // Prime anchors for the valid file.
        let anchors_ok = {
            let mut state = ctx.anchor_state.lock().unwrap();
            state.get_anchors(&file_ok).unwrap()
        };
        let (anchor, _) = &anchors_ok[0];

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "files": [
                        {
                            "path": file_ok.to_str().unwrap(),
                            "edits": [
                                {
                                    "anchor": anchor,
                                    "end_anchor": anchor,
                                    "text": "fn ok() { /* updated */ }"
                                }
                            ]
                        },
                        {
                            "path": "nonexistent.rs",
                            "edits": [
                                {
                                    "anchor": "whatever",
                                    "text": "nope"
                                }
                            ]
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        // Partial success: one file succeeded, one failed.
        assert!(
            !result.is_error,
            "expected partial success (ok), got error: {}",
            result.content
        );
        assert!(result.content.contains("Applied 1 edit(s)"));
        assert!(result.content.contains("ok.rs"));
        assert!(result.content.contains("edit_file:"));
        assert!(result.content.contains("nonexistent.rs"));

        assert_eq!(
            std::fs::read_to_string(&file_ok).unwrap(),
            "fn ok() { /* updated */ }\n"
        );
    }

    #[tokio::test]
    async fn multi_file_all_fail() {
        let dir = temp_dir("edit_all_fail");
        let ctx = test_context(dir);

        let tool = EditFileTool;
        let result = tool
            .execute(
                json!({
                    "files": [
                        {
                            "path": "nonexistent_a.rs",
                            "edits": [
                                {
                                    "anchor": "any",
                                    "text": "nope"
                                }
                            ]
                        },
                        {
                            "path": "nonexistent_b.rs",
                            "edits": [
                                {
                                    "anchor": "any",
                                    "text": "nope"
                                }
                            ]
                        }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        // All files failed → ToolResult::error.
        assert!(result.is_error, "expected error, got success");
        assert!(result.content.contains("nonexistent_a.rs"));
        assert!(result.content.contains("nonexistent_b.rs"));
    }

    // -----------------------------------------------------------------------
    // check_overlaps unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn check_overlaps_separate_ok() {
        let edits = vec![
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 0,
                end: 0,
                text: "replacement".into(),
            },
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 2,
                end: 3,
                text: "other".into(),
            },
        ];
        assert!(check_overlaps(&edits, "test.rs").is_ok());
    }

    #[test]
    fn check_overlaps_overlapping_replaces() {
        let edits = vec![
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 1,
                end: 4,
                text: "a".into(),
            },
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 3,
                end: 5,
                text: "b".into(),
            },
        ];
        let err = check_overlaps(&edits, "test.rs").unwrap_err();
        assert!(err.is_error);
        assert!(err.content.contains("Overlapping edit ranges in 'test.rs'"));
        assert!(err.content.contains("edit 0 (lines 2-5)"));
        assert!(err.content.contains("edit 1 (lines 4-6)"));
    }

    #[test]
    fn check_overlaps_replace_plus_insert_overlap() {
        // Replace [1,3] and insert_before at line 3 (range [3,3]) overlap.
        let edits = vec![
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 1,
                end: 3,
                text: "replacement".into(),
            },
            ResolvedEdit {
                operation: EditOp::InsertBefore,
                start: 3,
                end: 3,
                text: "inserted".into(),
            },
        ];
        let err = check_overlaps(&edits, "test.rs").unwrap_err();
        assert!(err.is_error);
        assert!(err.content.contains("Overlapping edit ranges in 'test.rs'"));
        assert!(err.content.contains("edit 0 (lines 2-4)"));
        assert!(err.content.contains("edit 1 (lines 4-4)"));
    }

    #[test]
    fn check_overlaps_adjacent_inserts_ok() {
        // insert_after(1) [1,1] and insert_before(2) [2,2] are adjacent but
        // disjoint — no overlap.
        let edits = vec![
            ResolvedEdit {
                operation: EditOp::InsertAfter,
                start: 1,
                end: 1,
                text: "a".into(),
            },
            ResolvedEdit {
                operation: EditOp::InsertBefore,
                start: 2,
                end: 2,
                text: "b".into(),
            },
        ];
        assert!(check_overlaps(&edits, "test.rs").is_ok());
    }

    #[test]
    fn check_overlaps_same_line_wrap_ok() {
        // insert_after(N) + insert_before(N) at the same line is a valid
        // "wrap this line" pattern — not an overlap.
        let edits = vec![
            ResolvedEdit {
                operation: EditOp::InsertAfter,
                start: 3,
                end: 3,
                text: "after".into(),
            },
            ResolvedEdit {
                operation: EditOp::InsertBefore,
                start: 3,
                end: 3,
                text: "before".into(),
            },
        ];
        assert!(check_overlaps(&edits, "test.rs").is_ok());
    }

    #[test]
    fn check_overlaps_identical_range() {
        // Two replaces targeting the same range — overlap.
        let edits = vec![
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 2,
                end: 5,
                text: "first".into(),
            },
            ResolvedEdit {
                operation: EditOp::Replace,
                start: 2,
                end: 5,
                text: "second".into(),
            },
        ];
        let err = check_overlaps(&edits, "test.rs").unwrap_err();
        assert!(err.is_error);
        assert!(err.content.contains("Overlapping edit ranges in 'test.rs'"));
    }

    #[test]
    fn check_overlaps_empty_list_ok() {
        assert!(check_overlaps(&[], "test.rs").is_ok());
    }
}
