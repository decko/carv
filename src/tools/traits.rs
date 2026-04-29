//! `Tool` trait — abstract interface for LLM-invokable tools.
//!
//! Each tool (e.g. `read_file`, `execute_command`) implements this trait to
//! expose a name, description, JSON Schema parameters, and an async `execute`
//! method. The agent loop calls `execute` and returns the [`ToolResult`] to
//! the LLM without knowing which tool is in use.
//!
//! ## Design
//! - Boxed-future return type (not RPITIT) keeps the trait **object-safe**,
//!   so callers can hold `dyn Tool` in a registry (`Vec<Box<dyn Tool>>`).
//!   This is the same pattern used by [`LlmProvider`](crate::llm::provider::LlmProvider).
//! - No `async-trait` crate — the boxed future is explicit.
//! - The [`ToolFuture`] type alias keeps the signature concise.
//! - Errors are returned via `anyhow::Result`. Tool-level errors (permission
//!   denied, file not found) are represented within [`ToolResult`] as
//!   `is_error: true` so the LLM can observe and recover.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Sendable, pinned future resolving to a [`ToolResult`].
///
/// The `'a` lifetime corresponds to the borrow of the tool's `&self` and
/// `&ToolContext`.  Using `dyn Future` (instead of `impl Future`) makes the
/// [`Tool`] trait object-safe.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Result of a single tool invocation.
///
/// `content` is a human-readable string returned to the LLM. `is_error`
/// signals that the tool encountered a recoverable problem (e.g. file not
/// found, parse failure) — the LLM can use this information to retry or
/// adjust its approach.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// The output text returned to the LLM.
    pub content: String,
    /// Whether the tool encountered an error.
    ///
    /// When `true`, `content` describes what went wrong. This is distinct
    /// from returning an `Err` from [`Tool::execute`], which indicates an
    /// unrecoverable failure (e.g. internal bug, configuration error).
    pub is_error: bool,
}

impl ToolResult {
    /// Create a successful tool result.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Create a tool result representing a recoverable error.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// Context shared across all tool invocations.
///
/// Carries project state such as the workspace root, LSP connection pool,
/// and permission configuration. This is a stub that will be expanded in
/// a future issue (#15).
#[derive(Debug, Default)]
pub struct ToolContext {}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A tool that the LLM can invoke.
///
/// The `execute` method returns a boxed future (not RPITIT `impl Future`)
/// to keep the trait object-safe, so callers can hold `Box<dyn Tool>` in a
/// registry. This is the same pattern used by [`LlmProvider`](crate::llm::provider::LlmProvider).
pub trait Tool: Send + Sync {
    /// Short identifier shown to the LLM (e.g. `"read_file"`).
    fn name(&self) -> &str;

    /// Human-readable description sent as the tool's `description` field.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's input parameters.
    fn parameters_schema(&self) -> Value;

    /// Whether this tool only reads data (informational, used in verbose output).
    /// Defaults to `false`.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Execute the tool with the given input and context.
    fn execute<'a>(&'a self, input: Value, ctx: &'a ToolContext) -> ToolFuture<'a>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct MockTool {
        content: String,
        should_error: bool,
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            "mock_tool"
        }

        fn description(&self) -> &str {
            "A mock tool for testing"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }

        fn is_read_only(&self) -> bool {
            true
        }

        fn execute<'a>(&'a self, _input: Value, _ctx: &'a ToolContext) -> ToolFuture<'a> {
            let content = self.content.clone();
            let is_error = self.should_error;
            Box::pin(async move {
                if is_error {
                    Ok(ToolResult::error(content))
                } else {
                    Ok(ToolResult::ok(content))
                }
            })
        }
    }

    #[tokio::test]
    async fn mock_tool_executes() {
        let tool = MockTool {
            content: "hello".into(),
            should_error: false,
        };
        let ctx = ToolContext::default();
        let result = tool.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(result.content, "hello");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn mock_tool_reports_error() {
        let tool = MockTool {
            content: "failed".into(),
            should_error: true,
        };
        let ctx = ToolContext::default();
        let result = tool.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(result.content, "failed");
        assert!(result.is_error);
        assert_eq!(result, ToolResult::error("failed"));
    }
}
