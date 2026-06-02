//! `search_files` tool — ripgrep-based content search with hash-anchored output.
//!
//! Walks files using `.gitignore`-aware traversal, finds lines matching a regex
//! pattern, and returns each matching line prefixed with its stable anchor word.
//! Anchors are the same format produced by `read_file`, so the LLM can reference
//! them in subsequent edit operations.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use grep_regex::RegexMatcher;
use grep_searcher::{sinks, Searcher};
use ignore::WalkBuilder;
use serde_json::Value;
use tracing::debug;

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};

/// Tool that searches file contents with a regex pattern and returns matching
/// lines with stable anchor identifiers.
///
/// This is the search counterpart to `ReadFileTool`. It respects `.gitignore`
/// rules and returns the same `{anchor}│{line}` format so the LLM can reference
/// results in edit calls.
pub struct SearchFilesTool;

impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search for a regex pattern across files in the project workspace. \
         Uses .gitignore-aware file traversal and returns matching lines \
         with stable anchor identifiers."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search, relative to the project root or absolute (defaults to workspace root)"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of matching lines to return (default 500)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            // Extract the "pattern" parameter (required).
            let pattern = match input.get("pattern").and_then(Value::as_str) {
                Some(p) => p,
                None => return Ok(ToolResult::error("missing required 'pattern' parameter")),
            };

            // Extract optional "path" parameter; default to workspace root.
            let search_path: PathBuf = match input.get("path").and_then(Value::as_str) {
                Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
                Some(p) => ctx.workspace_root.join(p),
                None => ctx.workspace_root.clone(),
            };

            // Extract optional "max_results" parameter; default to 500.
            let max_results: usize = input
                .get("max_results")
                .and_then(|v| v.as_u64())
                .map(|v| (v as usize).max(1))
                .unwrap_or(500);

            // Compile the regex pattern.
            let matcher = match RegexMatcher::new(pattern) {
                Ok(m) => m,
                Err(e) => return Ok(ToolResult::error(format!("invalid regex pattern: {e}"))),
            };

            // Walk files respecting .gitignore.
            let walker = WalkBuilder::new(&search_path)
                .standard_filters(true)
                .build();

            let mut searcher = Searcher::new();
            let mut all_results: Vec<String> = Vec::new();
            let mut truncated: usize = 0;

            for entry in walker {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        debug!("skipping unreadable entry: {e}");
                        continue;
                    }
                };

                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }

                let file_path = entry.path();

                // Use grep-searcher to find matching lines.
                let mut matched_line_nums: BTreeSet<u64> = BTreeSet::new();
                let search_result = searcher.search_path(
                    &matcher,
                    file_path,
                    sinks::UTF8(|line_num, _line| {
                        matched_line_nums.insert(line_num);
                        Ok::<_, std::io::Error>(true)
                    }),
                );

                // Skip files that can't be searched (binary, permission, etc.).
                if let Err(e) = search_result {
                    debug!("skipping unsearchable file {}: {e}", file_path.display());
                    continue;
                }

                if matched_line_nums.is_empty() {
                    continue;
                }

                // Cap check at the file level: once max_results is reached,
                // skip the anchor lock entirely — just count the omitted matches.
                if all_results.len() >= max_results {
                    truncated += matched_line_nums.len();
                    continue;
                }

                // Compute path relative to workspace root for output.
                let relative_path = file_path
                    .strip_prefix(&ctx.workspace_root)
                    .unwrap_or(file_path);

                // Get anchors for this file.
                let mut anchor_state = ctx.anchor_state.lock().expect("anchor state lock poisoned");
                let anchors = match anchor_state.get_anchors(file_path) {
                    Ok(a) => a,
                    Err(e) => {
                        debug!(
                            "skipping file with anchor error {}: {e}",
                            relative_path.display()
                        );
                        continue;
                    }
                };

                // Per-line: enforce cap within a file that straddles the boundary.
                for line_num in &matched_line_nums {
                    if all_results.len() >= max_results {
                        truncated += 1;
                    } else {
                        let idx = line_num.saturating_sub(1) as usize;
                        if let Some((anchor, line)) = anchors.get(idx) {
                            all_results
                                .push(format!("{}:{anchor}│{line}\n", relative_path.display()));
                        }
                    }
                }
            }

            if truncated > 0 {
                all_results.push(format!(
                    "... {truncated} more matches omitted. Narrow your search pattern.\n"
                ));
            }

            if all_results.is_empty() {
                return Ok(ToolResult::ok(format!(
                    "No matches found for pattern: {pattern}"
                )));
            }

            Ok(ToolResult::ok(all_results.concat()))
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
    use crate::tools::traits::ToolContext;
    use std::sync::{Arc, Mutex};

    /// Create (or re-use) a temporary directory inside the OS temp dir.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir); // clean stale state from prior runs
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
            parser_cache: Arc::new(Mutex::new(crate::treesitter::parser::ParserCache::new())),
        }
    }

    #[tokio::test]
    async fn search_matching_lines() {
        let dir = temp_dir("carv-test-search-matching");
        write_temp_file(
            &dir,
            "test.rs",
            "fn hello() {\n    let x = 42;\n    println!(\"hello\");\n}\n",
        );

        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            result.content.contains("test.rs:"),
            "missing file path prefix in:\n{}",
            result.content
        );
        assert!(
            result.content.contains("│fn hello() {"),
            "missing match in:\n{}",
            result.content
        );
        assert!(
            result.content.contains("│    println!(\"hello\");"),
            "missing match in:\n{}",
            result.content
        );
        // Every line must have a non-empty anchor word before the │.
        for line in result.content.lines() {
            assert!(line.contains('│'), "line missing │ separator: {line:?}");
            let before = line.split('│').next().unwrap_or("");
            // before is "path:anchor" — split on ':' and check the last segment
            let anchor_part = before.rsplitn(2, ':').next().unwrap_or("");
            assert!(
                !anchor_part.is_empty(),
                "anchor should not be empty in line: {line:?}"
            );
        }
    }

    #[tokio::test]
    async fn search_no_matches() {
        let dir = temp_dir("carv-test-search-none");
        write_temp_file(&dir, "data.txt", "hello world\n");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"pattern": "zzz_nonexistent"}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            result.content.contains("No matches found"),
            "expected 'No matches found' message, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn missing_pattern_parameter() {
        let dir = temp_dir("carv-test-missing-pattern");
        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap();

        assert!(result.is_error, "expected error for missing pattern");
        assert!(
            result.content.contains("missing required 'pattern'"),
            "error should mention missing pattern, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn pattern_is_not_a_string() {
        let dir = temp_dir("carv-test-pattern-not-string");
        let tool = SearchFilesTool;
        let ctx = test_context(dir);

        // `pattern` is a number, not a string.
        let result = tool
            .execute(serde_json::json!({"pattern": 42}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "expected error for non-string pattern");
        assert!(
            result.content.contains("missing required 'pattern'"),
            "error should mention missing pattern, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn invalid_regex_pattern() {
        let dir = temp_dir("carv-test-invalid-regex");
        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"pattern": "[unclosed"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "expected error for invalid regex");
        assert!(
            result.content.contains("invalid regex pattern"),
            "error should mention invalid regex, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn gitignore_respected() {
        let dir = temp_dir("carv-test-gitignore");

        // The `ignore` crate only applies gitignore rules within a git
        // repository. CI temp dirs are outside any repo, so we create a
        // fake .git directory to anchor gitignore resolution.
        std::fs::create_dir_all(dir.join(".git")).expect("create fake git repo");

        // Create a .gitignore that excludes bar.txt.
        write_temp_file(&dir, ".gitignore", "bar.txt\n");

        // Write files — one ignored, one not.
        write_temp_file(&dir, "foo.txt", "secret stuff\n");
        write_temp_file(&dir, "bar.txt", "secret stuff\n");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);

        let result = tool
            .execute(serde_json::json!({"pattern": "secret"}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );

        // Only foo.txt's content should appear (bar.txt is gitignored).
        // Both files have the same line content, so we count unique output lines.
        // With gitignore working we see 1 unique line; without it we'd see 2.
        let unique_lines: std::collections::HashSet<&str> = result.content.lines().collect();

        assert_eq!(
            unique_lines.len(),
            1,
            "expected exactly 1 unique line (gitignored file excluded), got {}: {:?}",
            unique_lines.len(),
            unique_lines
        );
        assert!(
            result.content.contains("foo.txt:"),
            "expected 'foo.txt:' file path prefix in output, got: {}",
            result.content
        );
        assert!(
            result.content.contains("│secret stuff"),
            "expected 'secret stuff' in output, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn path_parameter_scopes_search() {
        let dir = temp_dir("carv-test-scoped");

        // Two subdirectories.
        let sub_a = dir.join("a");
        let sub_b = dir.join("b");
        std::fs::create_dir_all(&sub_a).expect("failed to create sub_a");
        std::fs::create_dir_all(&sub_b).expect("failed to create sub_b");

        write_temp_file(&sub_a, "data.txt", "findme\n");
        write_temp_file(&sub_b, "data.txt", "findme\n");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);

        // Search only in subdirectory "a" using a relative path.
        let result = tool
            .execute(serde_json::json!({"pattern": "findme", "path": "a"}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            result.content.contains("a/data.txt:"),
            "expected 'a/data.txt:' file path prefix in output, got: {}",
            result.content
        );
        assert!(
            result.content.contains("│findme"),
            "expected match in output, got: {}",
            result.content
        );

        // Scoped to "a" should only give one result.
        let count = result.content.matches("│findme").count();
        assert_eq!(
            count, 1,
            "expected exactly 1 match (scoped to dir 'a'), got {}",
            count
        );
    }

    #[tokio::test]
    async fn empty_file_returns_no_matches() {
        let dir = temp_dir("carv-test-empty-search-file");
        write_temp_file(&dir, "empty.txt", "");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"pattern": "."}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            result.content.contains("No matches found"),
            "expected 'No matches found', got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn non_existent_path_returns_no_matches() {
        let dir = temp_dir("carv-test-bad-path");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(
                serde_json::json!({"pattern": "test", "path": "/nonexistent/path/12345"}),
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
            result.content.contains("No matches found"),
            "expected 'No matches found', got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn respects_max_results_within_single_file() {
        let dir = temp_dir("carv-test-max-results-file");
        // One file with 5 matching lines.
        write_temp_file(&dir, "data.txt", "match\nmatch\nmatch\nmatch\nmatch\n");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(
                serde_json::json!({"pattern": "match", "max_results": 2}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        // Exactly 2 matching lines.
        let match_count = result.content.matches("│match\n").count();
        assert_eq!(
            match_count, 2,
            "expected exactly 2 results, got {}: {}",
            match_count, result.content
        );
        // Truncation message mentions 3 omitted.
        assert!(
            result.content.contains("3 more matches omitted"),
            "expected truncation message mentioning 3 omitted, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn deduplicates_same_line_multiple_matches() {
        let dir = temp_dir("carv-test-dedup");
        // A line containing the same pattern twice — should only appear once.
        write_temp_file(&dir, "dup.txt", "hello hello world\n");

        let tool = SearchFilesTool;
        let ctx = test_context(dir);
        let result = tool
            .execute(serde_json::json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        assert!(
            result.content.contains("dup.txt:"),
            "expected 'dup.txt:' file path prefix in output, got: {}",
            result.content
        );
        // The line "hello hello world" should appear only once.
        let count = result.content.matches("│hello hello world").count();
        assert_eq!(
            count, 1,
            "expected line to appear once, got {} occurrences: {}",
            count, result.content
        );
    }
}
