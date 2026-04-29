// Per-file anchor state tracking with duplicate-line disambiguation.
//
// The [`AnchorState`] type manages anchor mappings for every file the
// agent has read or edited. It guarantees that:
//
// 1. Every line in a file gets a unique, deterministic anchor word.
// 2. Identical lines (e.g., multiple `}` or blank lines) are
//    disambiguated with occurrence-index suffixes: `Delta│}`,
//    `Delta.1│}`, `Delta.2│}`.
// 3. On file edit (`notify_edit`), the cache is invalidated so the
//    next read returns fresh anchors.
//
// # Occurrence-index semantics
//
// When two or more lines hash to the same base word, the **first**
// occurrence keeps the plain word. Subsequent occurrences append
// `.1`, `.2`, `.3`, … *not* `.0` — that keeps the common case
// (unique lines) clean of suffix noise.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use super::anchors::word_for_line;

// ---------------------------------------------------------------------------
// AnchorState
// ---------------------------------------------------------------------------

/// Tracks per-file anchor-to-line-content mappings for a session.
///
/// Anchors are deterministic — the same file always produces the same
/// anchors. Edits invalidate the cache for the changed file, forcing
/// a recompute on next access.
#[derive(Debug, Default)]
pub struct AnchorState {
    /// Map from file path → ordered list of `(anchor_word, line_content)`.
    files: HashMap<PathBuf, Vec<(String, String)>>,
}

impl AnchorState {
    /// Create an empty anchor state with no cached files.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the anchored lines for `path`.
    ///
    /// Reads the file from disk on first access and caches the result.
    /// Subsequent calls return the cached anchors. Returns owned data
    /// so callers can hold the result across mutable operations on
    /// `self`.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] if the file cannot be read (missing,
    /// permission denied, etc.).
    pub fn get_anchors(&mut self, path: &Path) -> io::Result<Vec<(String, String)>> {
        if let Some(cached) = self.files.get(path) {
            return Ok(cached.clone());
        }

        let content = std::fs::read_to_string(path)?;
        let anchors = compute_anchors(&content);
        self.files.insert(path.to_path_buf(), anchors.clone());
        Ok(anchors)
    }

    /// Invalidate the cached anchors for `path`.
    ///
    /// Call this after any write/edit tool modifies a file. The next
    /// call to [`get_anchors`] will re-read the file from disk and
    /// recompute anchors.
    pub fn notify_edit(&mut self, path: &Path) {
        self.files.remove(path);
    }

    /// Remove a file from anchor tracking entirely (e.g., after
    /// deletion). Same effect as [`notify_edit`] but semantically
    /// clearer for file deletions.
    pub fn remove(&mut self, path: &Path) {
        self.files.remove(path);
    }

    /// Return the number of files with cached anchor mappings.
    #[allow(dead_code)]
    pub(crate) fn file_count(&self) -> usize {
        self.files.len()
    }
}

// ---------------------------------------------------------------------------
// Anchor computation
// ---------------------------------------------------------------------------

/// Build a deduplicated list of `(anchor, line)` pairs from file content.
///
/// Each line is hashed via [`word_for_line`] to get a base anchor word.
/// When multiple lines produce the same base word, occurrence-index
/// suffixes are appended to guarantee uniqueness:
///
/// ```text
/// line  → word_for_line → base word
/// "}"   → "Delta"         → "Delta"      (first)
/// "}"   → "Delta"         → "Delta.1"    (second)
/// "}"   → "Delta"         → "Delta.2"    (third)
/// ```
fn compute_anchors(content: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    // Tracks how many times each base word has appeared so far.
    let mut counts: HashMap<&str, u32> = HashMap::new();

    for line in lines {
        let base = word_for_line(line);
        let count = counts.entry(base).or_insert(0);
        let anchor = if *count == 0 {
            base.to_string()
        } else {
            format!("{}.{}", base, *count)
        };
        *count += 1;
        result.push((anchor, line.to_string()));
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `content` to a temp file and return its path.
    fn temp_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("carv-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        path
    }

    #[test]
    fn empty_file_has_no_anchors() {
        let path = temp_file("empty.rs", "");
        let mut state = AnchorState::new();
        let anchors = state.get_anchors(&path).unwrap();
        assert!(anchors.is_empty());
    }

    #[test]
    fn unique_lines_get_unique_anchors() {
        let content = "fn one() {}\nfn two() {}\nfn three() {}\n";
        let path = temp_file("unique.rs", content);
        let mut state = AnchorState::new();
        let anchors = state.get_anchors(&path).unwrap();

        assert_eq!(anchors.len(), 3, "expected 3 lines, got {:?}", anchors);
        // All three lines are different → three distinct base words.
        let words: Vec<&str> = anchors.iter().map(|(a, _)| a.as_str()).collect();
        let unique: std::collections::HashSet<_> = words.iter().copied().collect();
        assert_eq!(
            unique.len(),
            3,
            "distinct lines should have distinct base words"
        );
    }

    #[test]
    fn duplicate_lines_get_occurrence_indices() {
        let content = "}\n}\n}\n";
        let path = temp_file("braces.rs", content);
        let mut state = AnchorState::new();
        let anchors = state.get_anchors(&path).unwrap();

        assert_eq!(anchors.len(), 3);
        let base = &anchors[0].0;
        assert!(
            !base.contains('.'),
            "first occurrence must not have dot: {base}"
        );

        let second = &anchors[1].0;
        assert_eq!(
            second,
            &format!("{}.1", base),
            "second must be {}.1: {second}",
            base
        );

        let third = &anchors[2].0;
        assert_eq!(
            third,
            &format!("{}.2", base),
            "third must be {}.2: {third}",
            base
        );
    }

    #[test]
    fn mixed_unique_and_duplicate_lines() {
        let content = "fn main() {\n    let x = 1;\n}\nfn other() {\n    let x = 1;\n}\n";
        let path = temp_file("mixed.rs", content);
        let mut state = AnchorState::new();
        let anchors = state.get_anchors(&path).unwrap();

        assert_eq!(anchors.len(), 6);
        // "    let x = 1;" appears twice → should get base and base.1
        let let_lines: Vec<_> = anchors
            .iter()
            .filter(|(_, l)| l == "    let x = 1;")
            .collect();
        assert_eq!(let_lines.len(), 2, "two identical let lines");
        assert!(!let_lines[0].0.contains('.'));
        assert!(let_lines[1].0.contains(".1"));
    }

    #[test]
    fn anchors_are_deterministic() {
        // Same file content → same anchors, every time.
        let path = temp_file("det.rs", "a\nb\nb\nc\n");
        let mut state = AnchorState::new();
        let first = state.get_anchors(&path).unwrap();

        // New state, same file → same output.
        let mut state2 = AnchorState::new();
        let second = state2.get_anchors(&path).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn anchors_match_line_content_one_to_one() {
        let lines = vec!["line0", "line1", "line2"];
        let content = lines.join("\n");
        let path = temp_file("lines.rs", &content);
        let mut state = AnchorState::new();
        let anchors = state.get_anchors(&path).unwrap();

        assert_eq!(anchors.len(), 3);
        for (i, (_, line)) in anchors.iter().enumerate() {
            assert_eq!(line, lines[i], "line {i} must match");
        }
    }

    #[test]
    fn notify_edit_invalidates_cache() {
        let path = temp_file("edit.rs", "original\n");
        let mut state = AnchorState::new();

        let first = state.get_anchors(&path).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, "original");

        // Edit the file on disk.
        std::fs::write(&path, "original\nmodified\n").unwrap();
        state.notify_edit(&path);

        let second = state.get_anchors(&path).unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(second[1].1, "modified", "should see the new line");
    }

    #[test]
    fn file_count_tracks_cached_files() {
        let a = temp_file("a.rs", "a\n");
        let b = temp_file("b.rs", "b\n");
        let mut state = AnchorState::new();

        assert_eq!(state.file_count(), 0);
        let _ = state.get_anchors(&a).unwrap();
        assert_eq!(state.file_count(), 1);
        let _ = state.get_anchors(&b).unwrap();
        assert_eq!(state.file_count(), 2);
        state.remove(&a);
        assert_eq!(state.file_count(), 1);
    }
}
