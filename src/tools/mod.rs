//! Tool registry — [`Tool`] trait, [`ToolResult`], and re-exports.
//!
//! Each tool the LLM can invoke (read_file, execute_command, etc.) implements
//! the [`Tool`] trait. The registry holds `Box<dyn Tool>` entries and applies
//! deny-list filtering. See the [design doc](crate) for the full tool inventory.

pub mod traits;

pub use traits::{Tool, ToolContext, ToolFuture, ToolResult};
