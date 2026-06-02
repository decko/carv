//! Tree-sitter language registry, grammar loading, and file-extension mapping.
//!
//! Grammar crates compile their own C source and expose language definitions
//! via `tree_sitter_language::LanguageFn` constants. The module maps file
//! extensions to [`Language`] variants and loads the corresponding grammar.

pub(crate) mod parser;

use std::path::Path;

// ---------------------------------------------------------------------------
// Language enum
// ---------------------------------------------------------------------------

/// Supported tree-sitter languages.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    /// Rust (`.rs`)
    Rust,
    /// Python (`.py`)
    Python,
    /// TypeScript (`.ts`, `.js`)
    TypeScript,
    /// TSX / React TypeScript (`.tsx`)
    Tsx,
}

// ---------------------------------------------------------------------------
// Grammar loading
// ---------------------------------------------------------------------------

/// Return the [`tree_sitter::Language`] for a supported [`Language`].
///
/// The grammar (`LanguageFn`) is converted into a `tree_sitter::Language`
/// via the `Into` impl provided by the `tree-sitter-language` crate.
pub fn language_grammar(lang: Language) -> tree_sitter::Language {
    match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    }
}

// ---------------------------------------------------------------------------
// File extension → language mapping
// ---------------------------------------------------------------------------

/// Map a file path to its tree-sitter [`Language`].
///
/// Extension matching is **case-insensitive** (`.rs`, `.RS`, `.Rs` all map
/// to Rust).  File names that *start* with a dot (e.g. `.rs`, `.tsconfig`)
/// have no extension per [`Path::extension`] semantics and return `None`.
///
/// # Examples
///
/// ```
/// # use carv::treesitter::{language_for_path, Language};
/// assert_eq!(language_for_path("src/main.rs"), Some(Language::Rust));
/// assert_eq!(language_for_path("script.py"), Some(Language::Python));
/// assert_eq!(language_for_path("index.ts"), Some(Language::TypeScript));
/// assert_eq!(language_for_path("app.js"), Some(Language::TypeScript));
/// assert_eq!(language_for_path("component.tsx"), Some(Language::Tsx));
/// assert_eq!(language_for_path("Makefile"), None);
/// ```
pub fn language_for_path(path: impl AsRef<Path>) -> Option<Language> {
    let ext = path.as_ref().extension()?.to_str()?;
    // Case-insensitive matching — `.RS`, `.Rs`, `.rs` all map to Rust.
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some(Language::Rust),
        "py" => Some(Language::Python),
        // tree-sitter-typescript handles both TS and JS syntax. JS-only
        // constructs (e.g. `with` statements) may parse with errors; a
        // dedicated `tree-sitter-javascript` crate would be needed for
        // fully-correct JS parsing (tracked in issue #4 for later).
        "ts" | "js" => Some(Language::TypeScript),
        "tsx" => Some(Language::Tsx),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- language_for_path ---------------------------------------------------

    #[test]
    fn rust_extension() {
        assert_eq!(language_for_path("main.rs"), Some(Language::Rust));
        assert_eq!(language_for_path("src/lib.rs"), Some(Language::Rust));
        // AsRef<Path> accepts PathBuf
        assert_eq!(
            language_for_path(std::path::PathBuf::from("mod.rs")),
            Some(Language::Rust)
        );
    }

    #[test]
    fn python_extension() {
        assert_eq!(language_for_path("script.py"), Some(Language::Python));
        assert_eq!(language_for_path("pkg/__init__.py"), Some(Language::Python));
    }

    #[test]
    fn typescript_extension() {
        assert_eq!(language_for_path("index.ts"), Some(Language::TypeScript));
        assert_eq!(language_for_path("lib/util.js"), Some(Language::TypeScript));
    }

    #[test]
    fn tsx_extension() {
        assert_eq!(language_for_path("App.tsx"), Some(Language::Tsx));
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(language_for_path("main.RS"), Some(Language::Rust));
        assert_eq!(language_for_path("script.PY"), Some(Language::Python));
        assert_eq!(language_for_path("index.Ts"), Some(Language::TypeScript));
    }

    #[test]
    fn unrecognized_extension() {
        assert_eq!(language_for_path("README.md"), None);
        assert_eq!(language_for_path("Makefile"), None);
        assert_eq!(language_for_path("main"), None);
    }

    #[test]
    fn empty_path() {
        assert_eq!(language_for_path(""), None);
    }

    // -- language_grammar ----------------------------------------------------

    #[test]
    fn grammars_are_loadable() {
        // Verify each grammar loads into a Parser and parses a minimal snippet.
        for (lang, variant, snippet) in [
            (Language::Rust, "Rust", "fn f() {}"),
            (Language::Python, "Python", "def f(): pass"),
            (Language::TypeScript, "TypeScript", "function f() {}"),
            (Language::Tsx, "Tsx", "const x = <div />;"),
        ] {
            let language = language_grammar(lang);
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&language)
                .unwrap_or_else(|_| panic!("{variant} grammar failed to load"));
            let tree = parser.parse(snippet, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{variant} grammar has parse errors on trivial input"
            );
        }
    }

    #[test]
    fn grammar_is_deterministic() {
        let a = language_grammar(Language::Rust);
        let b = language_grammar(Language::Rust);
        // Same grammar — parse the same input and compare trees.
        let mut p = tree_sitter::Parser::new();
        p.set_language(&a).unwrap();
        let tree_a = p.parse("fn main() {}", None).unwrap();
        p.set_language(&b).unwrap();
        let tree_b = p.parse("fn main() {}", None).unwrap();
        assert_eq!(tree_a.root_node().to_sexp(), tree_b.root_node().to_sexp());
    }
}
