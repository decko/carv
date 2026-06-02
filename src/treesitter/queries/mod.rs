//! Tree-sitter S-expression query files, embedded at compile time.
//!
//! Each language has a `.scm` file with `@definition.*` and `@name.*`
//! captures used by the `get_skeleton`, `get_function`, and `replace_symbol`
//! tools (G5, G6).

use super::Language;

// ---------------------------------------------------------------------------
// Embedded queries
// ---------------------------------------------------------------------------

/// Return the tag query for `language` as a static string.
///
/// The query captures definitions (`@definition.function`,
/// `@definition.class`, etc.) and references (`@name.reference`).
#[allow(dead_code)] // Used by G5/G6 tree-sitter tools.
pub fn query_for_language(lang: Language) -> &'static str {
    match lang {
        Language::Rust => include_str!("rust.scm"),
        Language::Python => include_str!("python.scm"),
        Language::TypeScript | Language::Tsx => include_str!("typescript.scm"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treesitter::language_grammar;
    use crate::treesitter::parser::ParserCache;
    use std::io::Write;
    use tree_sitter::{Query, StreamingIterator};

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("carv-test-queries");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        path
    }

    /// Run a query and return a sorted list of capture names.
    fn collect_captures(lang: Language, tree: &tree_sitter::Tree, content: &[u8]) -> Vec<String> {
        let query_src = query_for_language(lang);
        let language = language_grammar(lang);
        let query = Query::new(&language, query_src).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures: Vec<String> = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content);
        while let Some(m) = matches.next() {
            for cap in m.captures {
                captures.push(query.capture_names()[cap.index as usize].to_string());
            }
        }
        captures.sort();
        captures
    }

    #[test]
    fn rust_queries_capture_functions() {
        let path = temp_file("query_rust.rs", "fn hello() {}\nfn world() {}\n");
        let mut cache = ParserCache::new();
        let tree = cache.parse_file(&path).unwrap();
        let content = std::fs::read(&path).unwrap();

        let captures = collect_captures(Language::Rust, &tree, &content);
        let def_count = captures
            .iter()
            .filter(|c| c.as_str() == "definition.function")
            .count();
        assert_eq!(
            def_count, 2,
            "expected 2 function definitions, got: {:?}",
            captures
        );
    }

    #[test]
    fn rust_queries_capture_all_definition_types() {
        // Covers struct, enum, union, trait, impl, type, macro, module, const, static.
        let content_str = "struct S {}\nenum E {}\nunion U {}\ntrait T {}\nimpl T for S {}\ntype A = u8;\nmod m {}\npub const C: u8 = 0;\npub static S2: u8 = 0;\nmacro_rules! M { () => {} }\n";
        let path = temp_file("query_all_rs.rs", content_str);
        let mut cache = ParserCache::new();
        let tree = cache.parse_file(&path).unwrap();
        let content = std::fs::read(&path).unwrap();

        let captures = collect_captures(Language::Rust, &tree, &content);
        let count = |name: &str| captures.iter().filter(|c| c.as_str() == name).count();

        assert_eq!(count("definition.struct"), 1, "struct");
        assert_eq!(count("definition.enum"), 1, "enum");
        assert_eq!(count("definition.union"), 1, "union");
        assert_eq!(count("definition.trait"), 1, "trait");
        assert!(
            count("definition.impl") >= 1,
            "impl (expected at least 1, got {})",
            count("definition.impl")
        );
        assert_eq!(count("definition.type"), 1, "type alias");
        assert_eq!(count("definition.module"), 1, "module");
        assert_eq!(count("definition.constant"), 2, "const + static");
        assert_eq!(
            count("definition.macro"),
            1,
            "macro_rules, got: {:?}",
            captures
        );
    }

    #[test]
    fn python_queries_capture_functions_and_classes() {
        let content_str = "def foo():\n    pass\n\nclass Bar:\n    def baz(self):\n        pass\n";
        let path = temp_file("query_py.py", content_str);
        let mut cache = ParserCache::new();
        let tree = cache.parse_file(&path).unwrap();
        let content = std::fs::read(&path).unwrap();

        let captures = collect_captures(Language::Python, &tree, &content);
        let fn_count = captures
            .iter()
            .filter(|c| c.as_str() == "definition.function")
            .count();
        let class_count = captures
            .iter()
            .filter(|c| c.as_str() == "definition.class")
            .count();
        assert_eq!(
            fn_count, 2,
            "expected 2 function defs (foo + baz), got: {:?}",
            captures
        );
        assert_eq!(class_count, 1, "expected 1 class def, got: {:?}", captures);
    }

    #[test]
    fn typescript_queries_capture_function_and_class() {
        let content_str = "function hello() {}\nclass World {}\n";
        let path = temp_file("query_ts.ts", content_str);
        let mut cache = ParserCache::new();
        let tree = cache.parse_file(&path).unwrap();
        let content = std::fs::read(&path).unwrap();

        let captures = collect_captures(Language::TypeScript, &tree, &content);
        let fn_count = captures
            .iter()
            .filter(|c| c.as_str() == "definition.function")
            .count();
        let class_count = captures
            .iter()
            .filter(|c| c.as_str() == "definition.class")
            .count();
        assert_eq!(fn_count, 1, "expected 1 function def, got: {:?}", captures);
        assert_eq!(class_count, 1, "expected 1 class def, got: {:?}", captures);
    }
}
