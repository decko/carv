//! Tree-sitter parser cache with invalidation hooks.
//!
//! [`ParserCache`] manages per-file [`tree_sitter::Tree`] instances, avoiding
//! redundant parses. The cache is invalidated when tools modify files (via
//! `AnchorState::notify_edit` or direct calls to [`ParserCache::invalidate`]).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use super::{language_for_path, language_grammar};

// ---------------------------------------------------------------------------
// ParserCache
// ---------------------------------------------------------------------------

/// Per-file cache of parsed tree-sitter ASTs.
///
/// On first access, the file is read from disk, parsed via the appropriate
/// language grammar, and the resulting [`tree_sitter::Tree`] is stored.
/// Subsequent accesses return a clone of the cached tree.
///
/// When a tool modifies a file, call [`invalidate`](ParserCache::invalidate)
/// to remove the stale entry — the next access will re-read and re-parse.
#[derive(Debug, Default)]
pub struct ParserCache {
    /// Map from canonical file path → parsed tree.
    cache: HashMap<PathBuf, tree_sitter::Tree>,
}

impl ParserCache {
    /// Create an empty parser cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the parsed tree for `path`, re-parsing if not cached.
    ///
    /// The file extension determines which grammar to use. Unsupported
    /// extensions return an [`io::Error`] with kind [`InvalidInput`].
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
        if !self.cache.contains_key(path) {
            let tree = self.do_parse(path)?;
            self.cache.insert(path.to_path_buf(), tree);
        }
        // Tree::clone is a shallow copy (pointer clone to C-heap data).
        Ok(self.cache[path].clone())
    }

    /// Invalidate the cached tree for `path`.
    ///
    /// The next call to [`parse_file`] will re-read and re-parse.
    pub fn invalidate(&mut self, path: &Path) {
        self.cache.remove(path);
    }

    /// Return the number of files with cached parse trees.
    #[allow(dead_code)] // Only called from tests.
    pub(crate) fn cache_size(&self) -> usize {
        self.cache.len()
    }

    // -- internal helpers ----------------------------------------------------

    /// Read, determine language, and parse a file.
    fn do_parse(&self, path: &Path) -> io::Result<tree_sitter::Tree> {
        let path_str = path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 path"))?;

        let lang = language_for_path(path_str).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported file extension for '{}'", path_str),
            )
        })?;

        let content = std::fs::read_to_string(path)?;

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_grammar(lang))
            .expect("grammar load failed for valid language");

        parser
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
