//! Tree-sitter parser cache with invalidation hooks.
//!
//! [`ParserCache`] manages per-file [`tree_sitter::Tree`] instances, avoiding
//! redundant parses. The cache is invalidated by calling [`ParserCache::invalidate`]
//! — typically after a tool modifies a file.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use super::{language_for_path, language_grammar};

// ---------------------------------------------------------------------------
// ParserCache
// ---------------------------------------------------------------------------

/// Per-file cache of parsed tree-sitter ASTs.
///
/// On first access, the file is read from disk (as raw bytes — tree-sitter
/// accepts `&[u8]`, not just UTF-8), parsed via the appropriate language
/// grammar, and the resulting [`tree_sitter::Tree`] is stored.  Subsequent
/// accesses return a clone of the cached tree.
///
/// The parser is reused across invocations — a single [`tree_sitter::Parser`]
/// lives in the cache and is switched to the correct grammar on each parse.
///
/// Call [`invalidate`](ParserCache::invalidate) after a tool modifies a file
/// to force a re-parse on the next access.
pub struct ParserCache {
    /// Map from canonical file path → parsed tree.
    cache: HashMap<PathBuf, tree_sitter::Tree>,
    /// Reusable parser — language is switched on each parse.
    parser: tree_sitter::Parser,
}

impl std::fmt::Debug for ParserCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParserCache")
            .field("cached_files", &self.cache.len())
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)] // Methods used only from tests; external callers in G5-G6.
impl ParserCache {
    /// Create an empty parser cache with a fresh [`tree_sitter::Parser`].
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            parser: tree_sitter::Parser::new(),
        }
    }
}

// --- Default impl ---------------------------------------------------------

impl Default for ParserCache {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)] // Methods used only from tests; external callers in G5-G6.
impl ParserCache {
    /// Return the parsed tree for `path`, re-parsing if not cached.
    ///
    /// The file extension determines which grammar to use.  The path is
    /// canonicalized before cache lookup so that equivalent paths (e.g.
    /// `./foo.rs` and `/abs/foo.rs`) share cache entries.
    ///
    /// The file is read as raw bytes (`fs::read`) — tree-sitter accepts
    /// `&[u8]` directly, so non-UTF-8 files parse correctly.
    ///
    /// Returns a clone of the cached [`tree_sitter::Tree`] — the clone is
    /// cheap (pointer copy). The returned `Tree` is owned and can be used
    /// independently of the cache.
    ///
    /// # Errors
    ///
    /// * `io::ErrorKind::InvalidInput` — unsupported file extension.
    /// * `io::ErrorKind::NotFound` or other I/O errors from reading the file.
    /// * Parse failures return `io::ErrorKind::InvalidData`.
    ///
    /// # Panics
    ///
    /// Panics if the grammar `set_language` call fails after a valid
    /// `language_for_path` lookup (should be unreachable).
    pub fn parse_file(&mut self, path: &Path) -> io::Result<tree_sitter::Tree> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        // Parse first (borrows self.parser), then insert into cache — avoids
        // holding a self.cache borrow across the self.parser mutation.
        // tree_sitter::Parser does not impl Debug, so we manually impl it above.
        let needs_parse = !self.cache.contains_key(&canonical);
        if needs_parse {
            let tree = self.do_parse(path, &canonical)?;
            self.cache.insert(canonical.clone(), tree);
        }
        Ok(self.cache[&canonical].clone())
    }

    /// Invalidate the cached tree for `path`.
    ///
    /// The next call to [`parse_file`] will re-read and re-parse.
    pub fn invalidate(&mut self, path: &Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.cache.remove(&canonical);
    }

    /// Return the number of files with cached parse trees.
    pub(crate) fn cache_size(&self) -> usize {
        self.cache.len()
    }

    // -- internal helpers ----------------------------------------------------

    /// Read, determine language, and parse a file.
    fn do_parse(&mut self, path: &Path, canonical: &Path) -> io::Result<tree_sitter::Tree> {
        let path_str = canonical
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 path"))?;

        let lang = language_for_path(path_str).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported file extension for '{}'", path_str),
            )
        })?;

        let content = std::fs::read(path)?;

        self.parser
            .set_language(&language_grammar(lang))
            .expect("grammar load failed for valid language");

        self.parser
            .parse(&content, None)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "parse returned None"))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write content to a temp file and return its path.
    fn temp_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("carv-test-parser");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        path
    }

    #[test]
    fn parse_rust_file() {
        let path = temp_file("valid.rs", "fn main() {}\n");
        let mut cache = ParserCache::new();

        let tree = cache.parse_file(&path).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "valid Rust should parse clean"
        );
    }

    #[test]
    fn parse_python_file() {
        let path = temp_file("valid.py", "def f():\n    pass\n");
        let mut cache = ParserCache::new();

        let tree = cache.parse_file(&path).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "valid Python should parse clean"
        );
    }

    #[test]
    fn cache_hit_returns_same_tree() {
        let path = temp_file("cache_hit.rs", "fn main() {}\n");
        let mut cache = ParserCache::new();

        let tree1 = cache.parse_file(&path).unwrap();
        let tree2 = cache.parse_file(&path).unwrap();

        assert_eq!(tree1.root_node().to_sexp(), tree2.root_node().to_sexp());
        assert_eq!(cache.cache_size(), 1, "should only have one cached entry");
    }

    #[test]
    fn invalidate_triggers_reparse() {
        let path = temp_file("reparse.rs", "fn a() {}\n");
        let mut cache = ParserCache::new();

        let tree1 = cache.parse_file(&path).unwrap();
        assert!(!tree1.root_node().has_error());
        assert_eq!(cache.cache_size(), 1);

        // Modify the file on disk and invalidate.
        std::fs::write(&path, "fn b() {}\n").unwrap();
        assert_eq!(cache.cache_size(), 1, "cache still holds old tree");
        cache.invalidate(&path);
        assert_eq!(cache.cache_size(), 0, "cache cleared after invalidate");

        let tree2 = cache.parse_file(&path).unwrap();
        assert!(!tree2.root_node().has_error());
        assert_eq!(cache.cache_size(), 1, "re-parsed and cached");
    }

    #[test]
    fn unsupported_extension_errors() {
        let path = temp_file("readme.md", "# Hello\n");
        let mut cache = ParserCache::new();

        let err = cache.parse_file(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("unsupported file extension"));
    }

    #[test]
    fn cache_size_tracks_entries() {
        let a = temp_file("a.rs", "fn a() {}\n");
        let b = temp_file("b.py", "def b():\n    pass\n");
        let mut cache = ParserCache::new();

        assert_eq!(cache.cache_size(), 0);
        let _ = cache.parse_file(&a).unwrap();
        assert_eq!(cache.cache_size(), 1);
        let _ = cache.parse_file(&b).unwrap();
        assert_eq!(cache.cache_size(), 2);
        cache.invalidate(&a);
        assert_eq!(cache.cache_size(), 1);
    }

    #[test]
    fn file_not_found() {
        let path = PathBuf::from("/nonexistent/file.rs");
        let mut cache = ParserCache::new();
        let err = cache.parse_file(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
