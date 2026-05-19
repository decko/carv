//! Shared test utilities for tool integration tests.
//!
//! These helpers are used by multiple tool modules (fs, edit, etc.) to avoid
//! duplicating temp-dir management and context construction.

use crate::hashing::state::AnchorState;
use crate::tools::traits::ToolContext;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Create an empty temporary directory and return its path.
///
/// Removes any leftover directory from a previous run first, so tests
/// start with a clean slate regardless of state on disk.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("carv-test").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `content` to a file at `dir/name` and return the full path.
pub(crate) fn write_temp_file(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    path
}

/// Create a minimal [`ToolContext`] for a given workspace root.
pub(crate) fn test_context(workspace_root: PathBuf) -> ToolContext {
    ToolContext {
        workspace_root,
        anchor_state: Arc::new(Mutex::new(AnchorState::new())),
    }
}
