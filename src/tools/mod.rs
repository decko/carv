//! Tool registry — [`Tool`] trait, [`ToolResult`], and re-exports.
//!
//! Each tool the LLM can invoke (read_file, execute_command, etc.) implements
//! the [`Tool`] trait. The registry holds `Box<dyn Tool>` entries and applies
//! deny-list filtering. See the design doc for the full tool inventory.

pub mod exec;
pub mod fs;
pub mod registry;
pub mod search;
pub mod traits;

pub use registry::ToolRegistry;
pub use traits::{Tool, ToolContext, ToolFuture, ToolResult};

/// Returns the default set of tools available to the LLM.
///
/// The agent loop calls this to populate [`ToolRegistry`] at startup,
/// filtering out any disallowed tools afterward.
pub fn default_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(exec::ExecuteCommandTool),
        Box::new(fs::ReadFileTool),
        Box::new(search::SearchFilesTool),
    ]
}
