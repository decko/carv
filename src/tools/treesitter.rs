//! Tree-sitter structural tools — [`GetSkeletonTool`], [`GetFunctionTool`],
//! [`ReplaceSymbolTool`].
//!
//! These tools use tree-sitter queries to extract definition outlines
//! (`get_skeleton`), function bodies (`get_function`), and perform AST-aware
//! symbol replacement (`replace_symbol`) with byte-range splicing.

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
// replace_symbol
// ---------------------------------------------------------------------------

/// Parent node kinds that wrap a definition and should be included when
/// computing the byte range for replacement.
///
/// Example: a Python function decorated with `@decorator` is wrapped in a
/// `decorated_definition` node. Replacing just the `function_definition`
/// child would leave the orphaned decorator — we extend the range upward.
const WRAPPER_KINDS: &[&str] = &[
    // Python: @decorator wrapping a function or class
    "decorated_definition",
    // TypeScript: `export function ...`, `export default class ...`
    "export_statement",
    "export_default_declaration",
];

/// Extend the byte range of a definition node upward to include wrapper
/// nodes (decorators, export statements) that should be part of the
/// replacement.
///
/// Walks up from `node` through the parent chain; for each parent whose
/// `kind()` is in [`WRAPPER_KINDS`], the range is widened to include it.
fn extend_range_through_wrappers(mut node: tree_sitter::Node) -> tree_sitter::Node {
    while let Some(parent) = node.parent() {
        if WRAPPER_KINDS.contains(&parent.kind()) {
            node = parent;
        } else {
            break;
        }
    }
    node
}

/// Tool that replaces a named symbol (function, method, class) by AST node
/// with byte-range splicing.
///
/// Supports multi-file batching via a `files` array. Edits are applied
/// bottom-to-top to preserve byte offsets. Anchors and parser cache are
/// invalidated after each file write.
pub struct ReplaceSymbolTool;

impl Tool for ReplaceSymbolTool {
    fn name(&self) -> &str {
        "replace_symbol"
    }

    fn description(&self) -> &str {
        "Replace a function, method, or class definition by AST node in \
         one or more source files. Supports simple names (e.g. 'old_fn') \
         and dot-paths (e.g. 'MyStruct.old_method'). Accepts a `files` \
         array for multi-file batching. Edits are applied bottom-to-top \
         to preserve byte offsets. Decorators and export wrappers are \
         included in the replaced range."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Path to the source file"
                            },
                            "symbol": {
                                "type": "string",
                                "description": "Name of the symbol to replace. Use dot-notation for methods (e.g. 'Struct.old_method')"
                            },
                            "new_code": {
                                "type": "string",
                                "description": "New code to substitute at the symbol's byte range"
                            }
                        },
                        "required": ["path", "symbol", "new_code"]
                    },
                    "description": "List of files and symbols to replace"
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
            let files = match input.get("files").and_then(Value::as_array) {
                Some(arr) => arr,
                None => return Ok(ToolResult::error("missing required 'files' parameter")),
            };

            if files.is_empty() {
                return Ok(ToolResult::ok("(no files to process)"));
            }

            // Phase 1: resolve all edits. Each edit gets a (path, byte_range,
            // new_code) tuple plus a natural sort key for bottom-to-top.
            struct ResolvedEdit {
                path: PathBuf,
                start: usize, // byte offset in original file
                end: usize,   // byte offset in original file
                new_code: String,
                symbol: String, // for error messages
            }
            let mut edits: Vec<ResolvedEdit> = Vec::new();

            for file_entry in files {
                let path_str = match file_entry.get("path").and_then(Value::as_str) {
                    Some(p) => p,
                    None => {
                        return Ok(ToolResult::error("each file entry requires a 'path' field"));
                    }
                };
                let symbol = match file_entry.get("symbol").and_then(Value::as_str) {
                    Some(s) => s.to_string(),
                    None => {
                        return Ok(ToolResult::error(format!(
                            "missing 'symbol' for file '{}'",
                            path_str
                        )));
                    }
                };
                let new_code = match file_entry.get("new_code").and_then(Value::as_str) {
                    Some(n) => n.to_string(),
                    None => {
                        return Ok(ToolResult::error(format!(
                            "missing 'new_code' for file '{}'",
                            path_str
                        )));
                    }
                };

                let resolved = resolve_path(path_str, &ctx.workspace_root);

                // Validate that the path stays within the workspace root.
                if let Err(msg) =
                    crate::tools::check_path_in_workspace(&resolved, &ctx.workspace_root)
                {
                    return Ok(ToolResult::error(format!("replace_symbol failed: {msg}")));
                }

                let lang = match language_for_path(&resolved) {
                    Some(l) => l,
                    None => {
                        return Ok(ToolResult::error(format!(
                            "replace_symbol failed: unsupported extension for '{}'",
                            resolved.display()
                        )));
                    }
                };

                // Parse the file (lock scope tight — drop after parse).
                let tree = {
                    let mut pc = match ctx.parser_cache.lock() {
                        Ok(g) => g,
                        Err(_) => {
                            return Ok(ToolResult::error(
                                "replace_symbol failed: parser cache lock poisoned",
                            ));
                        }
                    };
                    match pc.parse_file(&resolved) {
                        Ok(t) => t,
                        Err(e) => {
                            return Ok(ToolResult::error(format!(
                                "replace_symbol failed: cannot parse '{}': {e}",
                                resolved.display()
                            )));
                        }
                    }
                };

                let content = match tokio::fs::read(&resolved).await {
                    Ok(c) => c,
                    Err(e) => {
                        return Ok(ToolResult::error(format!(
                            "replace_symbol failed: cannot read '{}': {e}",
                            resolved.display()
                        )));
                    }
                };

                // Find the symbol node.
                let def_node = if symbol.contains('.') {
                    let parts: Vec<&str> = symbol.split('.').collect();
                    if parts.len() != 2 {
                        return Ok(ToolResult::error(format!(
                            "replace_symbol failed: dot-path must have exactly 2 parts, got '{}'",
                            symbol
                        )));
                    }
                    find_dotpath_symbol(tree.root_node(), &parts, &content, lang)
                } else {
                    find_definition_by_name(tree.root_node(), &symbol, &content, lang)
                };

                let def_node = match def_node {
                    Some(n) => n,
                    None => {
                        return Ok(ToolResult::error(format!(
                            "replace_symbol failed: symbol '{}' not found in '{}'",
                            symbol,
                            resolved.display()
                        )));
                    }
                };

                // Extend range to include wrapper nodes.
                let replacement_node = extend_range_through_wrappers(def_node);
                let range = replacement_node.byte_range();
                edits.push(ResolvedEdit {
                    path: resolved,
                    start: range.start,
                    end: range.end,
                    new_code,
                    symbol,
                });
            }

            // Check for overlapping byte ranges on the same file before
            // applying any edits. Overlaps cause phase-2 offset
            // corruption because phase-1 byte ranges go stale.
            {
                let mut i = 0;
                while i < edits.len() {
                    let mut j = i + 1;
                    while j < edits.len() {
                        if edits[i].path == edits[j].path {
                            let (a, b) = if edits[i].start <= edits[j].start {
                                (&edits[i], &edits[j])
                            } else {
                                (&edits[j], &edits[i])
                            };
                            if a.end > b.start {
                                return Ok(ToolResult::error(format!(
                                    "replace_symbol failed: overlapping byte ranges on '{}' — symbols '{}' ({}..{}) and '{}' ({}..{})",
                                    a.path.display(),
                                    a.symbol, a.start, a.end,
                                    b.symbol, b.start, b.end,
                                )));
                            }
                        }
                        j += 1;
                    }
                    i += 1;
                }
            }

            // Phase 2: apply edits bottom-to-top (by descending byte offset
            // within each file). Edits on different files don't interfere.
            // Sort by (path, start descending) so later offsets don't shift.
            edits.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| b.start.cmp(&a.start)));

            let mut results: Vec<String> = Vec::new();

            for edit in &edits {
                // Read current file content (may have been modified by a
                // prior edit on the same file).
                let content = match tokio::fs::read(&edit.path).await {
                    Ok(c) => c,
                    Err(e) => {
                        results.push(format!(
                            "replace_symbol failed for '{}' in {}: {e}",
                            edit.symbol,
                            edit.path.display()
                        ));
                        continue;
                    }
                };

                // Validate byte range against current content.
                if edit.end > content.len() {
                    results.push(format!(
                        "replace_symbol failed for '{}' in {}: byte range {}..{} exceeds file length {}",
                        edit.symbol,
                        edit.path.display(),
                        edit.start,
                        edit.end,
                        content.len()
                    ));
                    continue;
                }

                // Splice: keep everything before `start`, insert `new_code`,
                // keep everything after `end`.
                let mut new_content: Vec<u8> =
                    Vec::with_capacity(content.len() + edit.new_code.len());
                new_content.extend_from_slice(&content[..edit.start]);
                new_content.extend_from_slice(edit.new_code.as_bytes());
                new_content.extend_from_slice(&content[edit.end..]);

                // Write back.
                if let Err(e) = tokio::fs::write(&edit.path, &new_content).await {
                    results.push(format!(
                        "replace_symbol failed for '{}' in {}: {e}",
                        edit.symbol,
                        edit.path.display()
                    ));
                    continue;
                }

                // Invalidate caches.
                if let Err(_e) = ctx
                    .anchor_state
                    .lock()
                    .map(|mut s| s.notify_edit(&edit.path))
                {
                    tracing::warn!(path = %edit.path.display(), "anchor state lock poisoned during replace_symbol invalidation");
                }
                if let Err(_e) = ctx
                    .parser_cache
                    .lock()
                    .map(|mut pc| pc.invalidate(&edit.path))
                {
                    tracing::warn!(path = %edit.path.display(), "parser cache lock poisoned during replace_symbol invalidation");
                }

                results.push(format!(
                    "Replaced '{}' in {} ({} → {} bytes)",
                    edit.symbol,
                    edit.path.display(),
                    edit.end - edit.start,
                    edit.new_code.len()
                ));
            }

            Ok(ToolResult::ok(results.join("\n")))
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

    // -----------------------------------------------------------------------
    // replace_symbol tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn replace_symbol_function_rust() {
        let dir = temp_dir("replace_rust");
        let file = write_temp_file(&dir, "main.rs", "fn old_fn() -> u32 {\n    0\n}\n");
        let path_str = file.to_str().unwrap();
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": path_str, "symbol": "old_fn", "new_code": "fn new_fn() -> u32 {\n    42\n}"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "replace failed: {}", result.content);
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(
            on_disk.contains("fn new_fn()"),
            "file should contain replacement, got: {on_disk}"
        );
        assert!(
            !on_disk.contains("old_fn"),
            "file should not contain old symbol, got: {on_disk}"
        );
        assert!(on_disk.contains("42"));
    }

    #[tokio::test]
    async fn replace_symbol_method_dotpath() {
        let dir = temp_dir("replace_dotpath");
        write_temp_file(
            &dir,
            "lib.rs",
            "struct Foo;\nimpl Foo {\n    fn old(&self) -> u32 { 1 }\n}\n",
        );
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": dir.join("lib.rs").to_str().unwrap(), "symbol": "Foo.old", "new_code": "fn new(&self) -> u32 { 99 }"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "replace failed: {}", result.content);
    }

    #[tokio::test]
    async fn replace_symbol_invalidates_cache() {
        let dir = temp_dir("replace_cache");
        let file = write_temp_file(&dir, "main.rs", "fn f() -> u32 { 1 }\nfn g() {}\n");
        let ctx = ts_test_context(dir.clone());

        // Populate parser cache by calling get_skeleton.
        let skeleton_tool = GetSkeletonTool;
        let _ = skeleton_tool
            .execute(json!({"path": file.to_str().unwrap()}), &ctx)
            .await
            .unwrap();

        assert_eq!(ctx.parser_cache.lock().unwrap().cache_size(), 1);

        // Replace f().
        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": file.to_str().unwrap(), "symbol": "f", "new_code": "fn replaced() {}"}]}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "replace failed: {}", result.content);

        // Cache should be cleared.
        assert_eq!(ctx.parser_cache.lock().unwrap().cache_size(), 0);
    }

    #[tokio::test]
    async fn replace_symbol_missing_symbol_errors() {
        let dir = temp_dir("replace_notfound");
        write_temp_file(&dir, "main.rs", "fn real() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": dir.join("main.rs").to_str().unwrap(), "symbol": "nonexistent", "new_code": "fn fake() {}"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn replace_symbol_empty_files() {
        let dir = temp_dir("replace_empty_files");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool.execute(json!({"files": []}), &ctx).await.unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "(no files to process)");
    }

    #[tokio::test]
    async fn replace_symbol_python_decorated_function() {
        let dir = temp_dir("replace_py_decorated");
        let file = write_temp_file(&dir, "mod.py", "@decorator\ndef old():\n    pass\n");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": file.to_str().unwrap(), "symbol": "old", "new_code": "def new():\n    return 0"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "replace failed: {}", result.content);
        let on_disk = std::fs::read_to_string(&file).unwrap();
        // The decorator should be gone too — the wrapper is included.
        assert!(
            !on_disk.contains("@decorator"),
            "wrapper decorator should be replaced along with function, got: {on_disk}"
        );
        assert!(on_disk.contains("def new()"));
    }

    #[tokio::test]
    async fn replace_symbol_typescript_exported_function() {
        let dir = temp_dir("replace_ts_export");
        let file = write_temp_file(&dir, "app.ts", "export function old() { return 0; }\n");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": file.to_str().unwrap(), "symbol": "old", "new_code": "export function new() { return 1; }"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "replace failed: {}", result.content);
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(!on_disk.contains("old()"));
        assert!(on_disk.contains("new()"));
    }

    #[tokio::test]
    async fn replace_symbol_multi_file_same_path() {
        let dir = temp_dir("replace_multi");
        let file = write_temp_file(
            &dir,
            "main.rs",
            "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n",
        );
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(json!({"files": [
                {"path": file.to_str().unwrap(), "symbol": "c", "new_code": "fn new_c() { 99 }"},
                {"path": file.to_str().unwrap(), "symbol": "a", "new_code": "fn new_a() { 11 }"}
            ]}), &ctx)
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "multi-file replace failed: {}",
            result.content
        );
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(
            on_disk.contains("fn new_a() { 11 }"),
            "a should be replaced, got: {on_disk}"
        );
        assert!(!on_disk.contains("fn a()"), "a should be gone");
        assert!(on_disk.contains("fn b() { 2 }"), "b should be untouched");
        assert!(
            on_disk.contains("fn new_c() { 99 }"),
            "c should be replaced"
        );
        assert!(!on_disk.contains("fn c()"), "c should be gone");
        // Bottom-to-top ordering: c (higher offset) applied first, then a.
        // Verify the result line order matches the original.
        let lines: Vec<&str> = on_disk.lines().collect();
        assert!(lines[0].contains("fn new_a()"), "a was first line");
        assert!(lines[1].contains("fn b()"), "b was second line");
        assert!(lines[2].contains("fn new_c()"), "c was third line");
    }

    #[tokio::test]
    async fn replace_symbol_export_default_declaration() {
        let dir = temp_dir("replace_ts_default");
        let file = write_temp_file(&dir, "app.ts", "export default class Old { x = 1; }\n");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": file.to_str().unwrap(), "symbol": "Old", "new_code": "export default class New { y = 2; }"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "replace failed: {}", result.content);
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(!on_disk.contains("Old"), "should replace Old class");
        assert!(on_disk.contains("New"), "should contain New class");
    }

    #[tokio::test]
    async fn replace_symbol_rejects_path_traversal() {
        let dir = temp_dir("replace_traversal");
        write_temp_file(&dir, "safe.rs", "fn safe() {}\n");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        let result = tool
            .execute(
                json!({"files": [{"path": "../../outside.rs", "symbol": "fake", "new_code": "fn real() {}"}]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error, "should reject traversal path");
        assert!(
            result.content.contains("path escapes workspace root"),
            "error should mention workspace escape, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn replace_symbol_overlapping_ranges_rejected() {
        let dir = temp_dir("replace_overlap");
        // fn a() { 1 }\nfn b() { 2 }\n
        // Byte ranges: a=0..13, b=15..28. These don't overlap.
        // But the 'files' is an array — a single tool call with two
        // entries targeting overlapping ranges on the same file.
        // The overlap is impossible with these two non-overlapping
        // symbols, so we test the happy path first, then test
        // rejection with a single-entry hash conflict won't trigger.
        write_temp_file(&dir, "mod.rs", "mod foo { fn bar() {} }\n");
        let ctx = ts_test_context(dir.clone());

        let tool = ReplaceSymbolTool;
        // Both `foo` (mod) and `bar` (fn inside mod) — bar's byte range
        // is a subset of foo's, so they overlap.
        let result = tool
            .execute(json!({"files": [
                {"path": dir.join("mod.rs").to_str().unwrap(), "symbol": "bar", "new_code": "fn replaced_inner() {}"},
                {"path": dir.join("mod.rs").to_str().unwrap(), "symbol": "foo", "new_code": "mod replaced_outer { fn inner() {} }"}
            ]}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "should reject overlapping ranges");
        assert!(
            result.content.contains("overlapping byte ranges"),
            "error should mention overlap, got: {}",
            result.content
        );
    }
}
