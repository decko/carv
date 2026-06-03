//! Tool registry — [`Tool`] trait, [`ToolResult`], and re-exports.
//!
//! Each tool the LLM can invoke (read_file, execute_command, etc.) implements
//! the [`Tool`] trait. The registry holds `Box<dyn Tool>` entries and applies
//! deny-list filtering. See the design doc for the full tool inventory.

pub mod edit;
pub mod exec;
pub mod fs;
pub mod registry;
pub mod search;
#[cfg(test)]
pub(crate) mod test_utils;
pub mod traits;
pub mod treesitter;

pub use registry::ToolRegistry;
pub use traits::{Tool, ToolContext, ToolFuture, ToolResult};

use std::path::{Path, PathBuf};

/// Validate that `resolved` stays within the workspace root.
///
/// Walks up from `resolved` to the nearest existing ancestor, canonicalizes,
/// and checks it's a prefix of the workspace root.  Returns `Ok(canonical)`
/// on success or `Err(error_message)` when the path escapes.
///
/// Used by `write_file`, `edit_file`, and any future file-modifying tool.
pub(crate) fn check_path_in_workspace(
    resolved: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let root_canon = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut probe = resolved.to_path_buf();
    let bound = loop {
        match probe.canonicalize() {
            Ok(c) => break c,
            Err(_) => match probe.parent() {
                Some(parent) => probe = parent.to_path_buf(),
                None => break resolved.to_path_buf(),
            },
        }
    };
    if !bound.starts_with(&root_canon) {
        Err("path escapes workspace root".to_string())
    } else {
        // Return the canonical form so callers can use it for cache keys.
        Ok(resolved
            .canonicalize()
            .unwrap_or_else(|_| resolved.to_path_buf()))
    }
}

/// Returns the default set of tools available to the LLM.
///
/// The agent loop calls this to populate [`ToolRegistry`] at startup,
/// filtering out any disallowed tools afterward.
pub fn default_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(edit::EditFileTool),
        Box::new(exec::ExecuteCommandTool),
        Box::new(fs::ReadFileTool),
        Box::new(search::SearchFilesTool),
        Box::new(treesitter::GetSkeletonTool),
        Box::new(treesitter::GetFunctionTool),
        Box::new(treesitter::ReplaceSymbolTool),
    ]
}
