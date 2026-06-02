//! Tree-sitter structural tools — [`GetSkeletonTool`], [`GetFunctionTool`].
//!
//! These tools use tree-sitter queries to extract definition outlines
//! (`get_skeleton`) and function bodies (`get_function`) from source files,
//! returning results with stable hash-anchored line references.

use std::io;
use std::path::{Path, PathBuf};
use std::str;

use serde_json::Value;
use tree_sitter::StreamingIterator;

use crate::tools::traits::{Tool, ToolContext, ToolFuture, ToolResult};
use crate::treesitter::{language_for_path, language_grammar, Language};

// ---------------------------------------------------------------------------
// Helpers: path resolution + file reading
// ---------------------------------------------------------------------------

/// Resolve and canonicalize a path relative to the workspace root.
fn resolve_path(path_str: &str, workspace_root: &Path) -> PathBuf {
    let resolved = if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        workspace_root.join(path_str)
    };
    resolved.canonicalize().unwrap_or(resolved)
}

/// Read raw file content as bytes (tree-sitter uses `&[u8]`).
fn read_file_bytes(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}

/// Convert raw bytes to a `&str`, returning a formatted error on failure.
fn to_str(content: &[u8]) -> io::Result<&str> {
    str::from_utf8(content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file is not valid UTF-8: {e}"),
        )
    })
}

// ---------------------------------------------------------------------------
// get_skeleton
// ---------------------------------------------------------------------------

/// Struct used internally to associate a definition line with its row number.
#[derive(Debug)]
struct DefLine {
    row: usize,
    line: String,
}

/// Collect distinct definition line numbers from a tree-sitter query.
///
/// Runs the language-appropriate definition query, collects start rows for
/// every `@definition.*` capture, deduplicates by row, and returns the
/// sorted list.
fn collect_definition_lines(
    lang: Language,
    tree: &tree_sitter::Tree,
    content: &[u8],
) -> io::Result<Vec<DefLine>> {
    let query_src = crate::treesitter::queries::query_for_language(lang);
    let language = language_grammar(lang);
    let query = tree_sitter::Query::new(&language, query_src).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("query compilation failed: {e}"),
        )
    })?;
    let mut cursor = tree_sitter::QueryCursor::new();
    let content_str = to_str(content)?;

    let mut rows: Vec<DefLine> = Vec::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let capture_name = query.capture_names()[cap.index as usize];
            if capture_name.starts_with("definition.") {
                let row = cap.node.start_position().row;
                // Avoid duplicates: same row + same line from multiple captures.
                let line = content_str.lines().nth(row).unwrap_or("").to_string();
                rows.push(DefLine { row, line });
            }
        }
    }

    // Deduplicate by row, keeping first occurrence.
    rows.sort_by_key(|d| d.row);
    rows.dedup_by_key(|d| d.row);

    Ok(rows)
}

/// Format a list of definition lines with hash-anchored references.
///
/// Uses [`AnchorState`] to get occurrence-indexed anchors for the full
/// file, then returns only the definition rows. This guarantees anchors
/// match `read_file` output (Invariant #3).
fn format_skeleton_with_anchors(lines: &[DefLine], file_anchors: &[(String, String)]) -> String {
    let mut output = String::new();
    for def in lines {
        if let Some((anchor, _)) = file_anchors.get(def.row) {
            output.push_str(&format!("{anchor}│{}\n", def.line));
        }
    }
    output
}

/// Tool that returns a structural outline of a source file.
///
/// Parses the file with tree-sitter, runs the language-appropriate definition
/// query, and returns one anchored line per definition (function, struct,
/// class, etc.). Results are sorted by line order and deduplicated.
pub struct GetSkeletonTool;

impl Tool for GetSkeletonTool {
    fn name(&self) -> &str {
        "get_skeleton"
    }

    fn description(&self) -> &str {
        "Return an AST structural outline of a file. \
         Shows the signature line for each top-level definition \
         (functions, structs, classes, impls, etc.) with stable anchor \
         identifiers for each line. Useful for understanding file structure \
         before reading or editing specific sections."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the source file, relative to the project root or absolute"
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

            let resolved = resolve_path(path_str, &ctx.workspace_root);

            // Determine language.
            let lang = match language_for_path(&resolved) {
                Some(l) => l,
                None => {
                    return Ok(ToolResult::error(format!(
                        "get_skeleton failed: unsupported file extension for '{}'",
                        resolved.display()
                    )));
                }
            };

            // Parse the file. Lock parser cache only for the parse; release
            // immediately afterward so we don't hold the lock during anchor
            // state operations and file I/O.
            let tree = {
                let mut parser_cache = match ctx.parser_cache.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        return Ok(ToolResult::error(
                            "get_skeleton failed: parser cache lock poisoned",
                        ));
                    }
                };
                match parser_cache.parse_file(&resolved) {
                    Ok(t) => t,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "get_skeleton failed: cannot parse '{}': {e}",
                            resolved.display()
                        )));
                    }
                }
            };

            // Read content and collect definition lines.
            let content = match read_file_bytes(&resolved) {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolResult::error(format!(
                        "get_skeleton failed: cannot read '{}': {e}",
                        resolved.display()
                    )));
                }
            };

            let defs = match collect_definition_lines(lang, &tree, &content) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(ToolResult::error(format!("get_skeleton failed: {e}")));
                }
            };

            if defs.is_empty() {
                return Ok(ToolResult::ok("(no definitions found)"));
            }

            // Get full file anchors (with occurrence indices) so output
            // matches read_file and works with edit_file.
            let mut anchor_state = match ctx.anchor_state.lock() {
                Ok(s) => s,
                Err(_) => {
                    return Ok(ToolResult::error(
                        "get_skeleton failed: anchor state lock poisoned",
                    ));
                }
            };
            let file_anchors = match anchor_state.get_anchors(&resolved) {
                Ok(a) => a,
                Err(e) => {
                    return Ok(ToolResult::error(format!(
                        "get_skeleton failed: cannot read anchors for '{}': {e}",
                        resolved.display()
                    )));
                }
            };

            Ok(ToolResult::ok(format_skeleton_with_anchors(
                &defs,
                &file_anchors,
            )))
        })
    }
}

// ---------------------------------------------------------------------------
// get_function
// ---------------------------------------------------------------------------

/// Body node types per language.
///
/// After finding a definition node (e.g. `function_item`), walking its
/// children for one of these node kinds yields the body.
const RUST_BODY_KINDS: &[&str] = &["block", "declaration_list"];
const PYTHON_BODY_KINDS: &[&str] = &["block"];
const TYPESCRIPT_BODY_KINDS: &[&str] = &["statement_block", "class_body"];

/// Return the body node kinds for the given language.
fn body_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => RUST_BODY_KINDS,
        Language::Python => PYTHON_BODY_KINDS,
        Language::TypeScript | Language::Tsx => TYPESCRIPT_BODY_KINDS,
    }
}

/// Definition node kinds per language.
///
/// Only nodes of these types are considered when searching for a symbol.
const RUST_DEF_KINDS: &[&str] = &[
    "function_item",
    "struct_item",
    "enum_item",
    "union_item",
    "trait_item",
    "impl_item",
    "type_item",
    "mod_item",
];
const PYTHON_DEF_KINDS: &[&str] = &["function_definition", "class_definition"];
const TYPESCRIPT_DEF_KINDS: &[&str] = &[
    "function_declaration",
    "method_definition",
    "class_declaration",
    "abstract_class_declaration",
    "interface_declaration",
];

/// Return the definition node kinds for the given language.
fn def_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => RUST_DEF_KINDS,
        Language::Python => PYTHON_DEF_KINDS,
        Language::TypeScript | Language::Tsx => TYPESCRIPT_DEF_KINDS,
    }
}

/// Name child kind per language.
const RUST_NAME_KINDS: &[&str] = &["identifier", "type_identifier"];
const PYTHON_NAME_KINDS: &[&str] = &["identifier"];
const TYPESCRIPT_NAME_KINDS: &[&str] = &["identifier", "type_identifier", "property_identifier"];

/// Return the name child kinds for the given language.
fn name_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => RUST_NAME_KINDS,
        Language::Python => PYTHON_NAME_KINDS,
        Language::TypeScript | Language::Tsx => TYPESCRIPT_NAME_KINDS,
    }
}

/// Extract the name text from a definition node.
///
/// Walks the node's named children looking for an identifier-type node,
/// returns its UTF-8 text if found.
fn extract_name<'a>(node: tree_sitter::Node, content: &'a [u8], lang: Language) -> Option<&'a str> {
    let kinds = name_kinds(lang);
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.is_named() && kinds.contains(&child.kind()) {
                return child.utf8_text(content).ok();
            }
        }
    }
    None
}

/// Find a body node inside a definition node.
///
/// Walks named children looking for a body-type node (`block`,
/// `statement_block`, etc.) and returns it.
fn find_body_node(node: tree_sitter::Node, lang: Language) -> Option<tree_sitter::Node> {
    let kinds = body_kinds(lang);
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.is_named() && kinds.contains(&child.kind()) {
                return Some(child);
            }
        }
    }
    None
}

/// Find a definition node whose name matches `target`, searching recursively
/// from `node`.
///
/// Returns the definition node (not its body) on success.
fn find_definition_by_name<'a>(
    node: tree_sitter::Node<'a>,
    target: &str,
    content: &[u8],
    lang: Language,
) -> Option<tree_sitter::Node<'a>> {
    let kinds = def_kinds(lang);

    for i in 0..node.child_count() {
        let child = match node.child(i as u32) {
            Some(c) => c,
            None => continue,
        };

        if child.is_named() && kinds.contains(&child.kind()) {
            if let Some(name) = extract_name(child, content, lang) {
                if name == target {
                    return Some(child);
                }
            }
        }

        // Recurse into children for nested definitions.
        if let Some(result) = find_definition_by_name(child, target, content, lang) {
            return Some(result);
        }
    }

    None
}

/// Find a symbol via dot-path (e.g. `"Foo.bar"`).
///
/// Splits the symbol by `.`, finds the first-part definition, then searches
/// inside it for the second-part definition. Returns the innermost definition
/// node (containing the method body), or `None` if not found.
fn find_dotpath_symbol<'a>(
    node: tree_sitter::Node<'a>,
    parts: &[&str],
    content: &[u8],
    lang: Language,
) -> Option<tree_sitter::Node<'a>> {
    if parts.is_empty() {
        return None;
    }

    let kinds = def_kinds(lang);
    let outer_name = parts[0];
    let inner_name = parts.get(1).copied().unwrap_or("");

    for i in 0..node.child_count() {
        let child = match node.child(i as u32) {
            Some(c) => c,
            None => continue,
        };

        if child.is_named() && kinds.contains(&child.kind()) {
            if let Some(name) = extract_name(child, content, lang) {
                if name == outer_name {
                    // Found the outer definition. Search inside it for the inner.
                    if let Some(result) = find_definition_by_name(child, inner_name, content, lang)
                    {
                        return Some(result);
                    }
                    // Fall through: the outer match doesn't contain the inner
                    // symbol. Continue searching for another outer match
                    // (e.g., `struct Foo;` has no body, but `impl Foo { ... }`
                    // does — both match the outer name).
                }
            }
        }

        // Recurse.
        if let Some(result) = find_dotpath_symbol(child, parts, content, lang) {
            return Some(result);
        }
    }

    None
}

/// Tool that extracts a function body by symbol name.
///
/// Finds a named function/method via its symbol (e.g. `"main"` or
/// `"MyStruct.my_method"`), extracts the body text with byte-range accuracy,
/// and returns it with stable hash-anchored line references.
pub struct GetFunctionTool;

impl Tool for GetFunctionTool {
    fn name(&self) -> &str {
        "get_function"
    }

    fn description(&self) -> &str {
        "Extract the body of a named function or method from a source file. \
         Accepts a simple name (e.g. 'main') or a dot-path (e.g. \
         'MyStruct.my_method'). Returns the body text with stable anchor \
         identifiers for each line."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the source file, relative to the project root or absolute"
                },
                "symbol": {
                    "type": "string",
                    "description": "Name of the function/method to extract. Use dot-notation for methods (e.g. 'MyStruct.my_method')"
                }
            },
            "required": ["path", "symbol"]
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

            let symbol = match input.get("symbol").and_then(Value::as_str) {
                Some(s) => s,
                None => return Ok(ToolResult::error("missing required 'symbol' parameter")),
            };

            let resolved = resolve_path(path_str, &ctx.workspace_root);

            let lang = match language_for_path(&resolved) {
                Some(l) => l,
                None => {
                    return Ok(ToolResult::error(format!(
                        "get_function failed: unsupported file extension for '{}'",
                        resolved.display()
                    )));
                }
            };

            // Parse the file. Lock parser cache only for the parse; release
            // immediately afterward so we don't hold the lock during anchor
            // state operations and file I/O.
            let tree = {
                let mut parser_cache = match ctx.parser_cache.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        return Ok(ToolResult::error(
                            "get_function failed: parser cache lock poisoned",
                        ));
                    }
                };
                match parser_cache.parse_file(&resolved) {
                    Ok(t) => t,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "get_function failed: cannot parse '{}': {e}",
                            resolved.display()
                        )));
                    }
                }
            };

            let content = match read_file_bytes(&resolved) {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolResult::error(format!(
                        "get_function failed: cannot read '{}': {e}",
                        resolved.display()
                    )));
                }
            };

            // Find the symbol: try dot-path first, then single name.
            let def_node = if symbol.contains('.') {
                let parts: Vec<&str> = symbol.split('.').collect();
                if parts.len() != 2 {
                    return Ok(ToolResult::error(format!(
                        "get_function failed: dot-path must have exactly 2 parts (e.g. 'Class.method'), got '{}'",
                        symbol
                    )));
                }
                find_dotpath_symbol(tree.root_node(), &parts, &content, lang)
            } else {
                find_definition_by_name(tree.root_node(), symbol, &content, lang)
            };

            let def_node = match def_node {
                Some(n) => n,
                None => {
                    return Ok(ToolResult::error(format!(
                        "get_function failed: symbol '{}' not found in '{}'",
                        symbol,
                        resolved.display()
                    )));
                }
            };

            // Extract the body node.
            let body_node = match find_body_node(def_node, lang) {
                Some(b) => b,
                None => {
                    return Ok(ToolResult::error(format!(
                        "get_function failed: symbol '{}' has no body (may be a forward declaration or trait definition)",
                        symbol
                    )));
                }
            };

            // Extract body bytes by range.
            let body_bytes = &content[body_node.byte_range()];
            let body_str = match to_str(body_bytes) {
                Ok(s) => s,
                Err(e) => {
                    return Ok(ToolResult::error(format!("get_function failed: {e}")));
                }
            };

            if body_str.trim().is_empty() {
                return Ok(ToolResult::ok("(empty body)"));
            }

            // Use AnchorState for occurrence-indexed anchors that match
            // read_file output (Invariant #3). Index into the full file's
            // anchor list by the body node's row range.
            let mut anchor_state = match ctx.anchor_state.lock() {
                Ok(s) => s,
                Err(_) => {
                    return Ok(ToolResult::error(
                        "get_function failed: anchor state lock poisoned",
                    ));
                }
            };
            let file_anchors = match anchor_state.get_anchors(&resolved) {
                Ok(a) => a,
                Err(e) => {
                    return Ok(ToolResult::error(format!(
                        "get_function failed: cannot read anchors for '{}': {e}",
                        resolved.display()
                    )));
                }
            };

            let start_row = body_node.start_position().row;
            let end_row = body_node.end_position().row;

            let mut output = String::new();
            for row in start_row..=end_row {
                if let Some((anchor, _)) = file_anchors.get(row) {
                    let line = body_str.lines().nth(row - start_row).unwrap_or("");
                    output.push_str(&format!("{anchor}│{line}\n"));
                }
            }

            Ok(ToolResult::ok(output))
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
    use crate::tools::test_utils::{temp_dir, write_temp_file};
    use crate::treesitter::parser::ParserCache;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// Create a `ToolContext` with a fresh `ParserCache`.
    fn ts_test_context(workspace_root: PathBuf) -> ToolContext {
        ToolContext {
            workspace_root,
            anchor_state: Arc::new(Mutex::new(AnchorState::new())),
            parser_cache: Arc::new(Mutex::new(ParserCache::new())),
        }
    }

    // -----------------------------------------------------------------------
    // get_skeleton tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn skeleton_rust_returns_functions() {
        let dir = temp_dir("skeleton_rust");
        write_temp_file(&dir, "lib.rs", "fn hello() {}\nfn world() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(json!({"path": dir.join("lib.rs").to_str().unwrap()}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "skeleton failed: {}", result.content);
        assert!(result.content.contains("fn hello() {}"));
        assert!(result.content.contains("fn world() {}"));
    }

    #[tokio::test]
    async fn skeleton_rust_shows_structs() {
        let dir = temp_dir("skeleton_struct");
        write_temp_file(&dir, "types.rs", "pub struct Config { verbose: bool }\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(
                json!({"path": dir.join("types.rs").to_str().unwrap()}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "skeleton failed: {}", result.content);
        assert!(result.content.contains("pub struct Config"));
    }

    #[tokio::test]
    async fn skeleton_python_shows_class_and_functions() {
        let dir = temp_dir("skeleton_py");
        write_temp_file(
            &dir,
            "mod.py",
            "def foo():\n    pass\n\nclass Bar:\n    def baz(self): pass\n",
        );
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(json!({"path": dir.join("mod.py").to_str().unwrap()}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "skeleton failed: {}", result.content);
        assert!(result.content.contains("def foo():"));
        assert!(result.content.contains("class Bar:"));
        // Method baz is inside class — tree-sitter scoping may or may not catch it
        // on its own line. We assert that at least class and top-level function appear.
    }

    #[tokio::test]
    async fn skeleton_typescript_shows_functions() {
        let dir = temp_dir("skeleton_ts");
        write_temp_file(&dir, "app.ts", "function start() {}\nclass App {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(json!({"path": dir.join("app.ts").to_str().unwrap()}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "skeleton failed: {}", result.content);
        assert!(result.content.contains("function start()"));
        assert!(result.content.contains("class App"));
    }

    #[tokio::test]
    async fn skeleton_output_has_anchors() {
        let dir = temp_dir("skeleton_anchors");
        write_temp_file(&dir, "main.rs", "fn alpha() {}\nfn beta() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(json!({"path": dir.join("main.rs").to_str().unwrap()}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "skeleton failed: {}", result.content);
        // Each output line should have the anchor│content format.
        for line in result.content.lines() {
            assert!(
                line.contains('│'),
                "expected anchor separator in line: '{line}'"
            );
        }
    }

    #[tokio::test]
    async fn skeleton_unsupported_extension() {
        let dir = temp_dir("skeleton_bad_ext");
        write_temp_file(&dir, "readme.md", "# Hello\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(
                json!({"path": dir.join("readme.md").to_str().unwrap()}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("unsupported file extension"));
    }

    #[tokio::test]
    async fn skeleton_empty_file() {
        let dir = temp_dir("skeleton_empty");
        write_temp_file(&dir, "empty.rs", "");
        let ctx = ts_test_context(dir.clone());

        let tool = GetSkeletonTool;
        let result = tool
            .execute(
                json!({"path": dir.join("empty.rs").to_str().unwrap()}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "(no definitions found)");
    }

    // -----------------------------------------------------------------------
    // get_function tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_function_rust_returns_body() {
        let dir = temp_dir("get_fn_rust");
        write_temp_file(
            &dir,
            "main.rs",
            "fn main() {\n    let x = 42;\n    println!(\"Hello\");\n}\n",
        );
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("main.rs").to_str().unwrap(), "symbol": "main"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "get_function failed: {}", result.content);
        assert!(
            result.content.contains("let x = 42;"),
            "body should contain let binding, got: {}",
            result.content
        );
        assert!(
            result.content.contains("println!"),
            "body should contain macro call, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn get_function_rust_method_via_dotpath() {
        let dir = temp_dir("get_fn_dotpath");
        write_temp_file(
            &dir,
            "impl.rs",
            "struct Foo;\nimpl Foo {\n    fn bar(&self) -> u32 {\n        42\n    }\n}\n",
        );
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("impl.rs").to_str().unwrap(), "symbol": "Foo.bar"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "get_function dot-path failed: {}",
            result.content
        );
        assert!(
            result.content.contains("42"),
            "body should contain '42', got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn get_function_python_method_via_dotpath() {
        let dir = temp_dir("get_fn_py_dotpath");
        write_temp_file(
            &dir,
            "mod.py",
            "class Calculator:\n    def add(self, a, b):\n        return a + b\n",
        );
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("mod.py").to_str().unwrap(), "symbol": "Calculator.add"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "get_function py dot-path failed: {}",
            result.content
        );
        assert!(
            result.content.contains("return a + b"),
            "body should contain return statement, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn get_function_returns_anchors() {
        let dir = temp_dir("get_fn_anchors");
        write_temp_file(&dir, "main.rs", "fn calc() {\n    1 + 1\n}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("main.rs").to_str().unwrap(), "symbol": "calc"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "get_function failed: {}", result.content);
        assert!(
            result.content.contains('│'),
            "output should contain anchors, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn get_function_symbol_not_found() {
        let dir = temp_dir("get_fn_notfound");
        write_temp_file(&dir, "main.rs", "fn main() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("main.rs").to_str().unwrap(), "symbol": "nonexistent"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn get_function_empty_body() {
        let dir = temp_dir("get_fn_empty");
        write_temp_file(&dir, "main.rs", "fn empty_fn() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("main.rs").to_str().unwrap(), "symbol": "empty_fn"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        // The body of `fn empty_fn() {}` is `{}` (a single brace pair with
        // no content). The tool returns this as anchored lines, not as
        // "(empty body)", because `{}` is real (trivial) content.
        assert!(
            result.content.contains("{}"),
            "expected body with braces, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn get_function_missing_symbol_parameter() {
        let dir = temp_dir("get_fn_missing_sym");
        write_temp_file(&dir, "main.rs", "fn main() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(json!({"path": dir.join("main.rs").to_str().unwrap()}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert_eq!(result.content, "missing required 'symbol' parameter");
    }

    #[tokio::test]
    async fn get_function_three_part_dotpath_rejected() {
        let dir = temp_dir("get_fn_3part");
        write_temp_file(&dir, "main.rs", "fn main() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = GetFunctionTool;
        let result = tool
            .execute(
                json!({"path": dir.join("main.rs").to_str().unwrap(), "symbol": "A.B.C"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("must have exactly 2 parts"));
    }
}
