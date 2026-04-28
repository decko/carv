//! `LlmProvider` trait — abstract interface for LLM backends.
//!
//! Each provider (Anthropic, OpenAI) implements this trait to expose a
//! unified streaming chat interface. The agent loop calls `stream_chat` and
//! consumes [`LlmEvent`] items without knowing which backend is in use.
//!
//! ## Design
//! - Boxed-future return type (not RPITIT) keeps the trait **object-safe**,
//!   so callers can hold `dyn LlmProvider` behind an `Arc`.  This is the same
//!   pattern used by [`StreamOutput`](crate::stream::output::StreamOutput).
//! - No `async-trait` crate — the boxed future is explicit.
//! - The [`LlmStream`] and [`LlmStreamFuture`] aliases (from
//!   [`crate::llm::types`]) keep the signature concise.
//! - Errors are returned via `anyhow::Result` at both the stream and item levels.

use crate::llm::types::{LlmStreamFuture, Message, RequestConfig, ToolDef};

/// Abstract LLM provider capable of streaming chat completions with tool use.
///
/// Implementations handle the provider-specific wire protocol (SSE event
/// parsing, authentication, request formatting) and emit a unified stream
/// of [`LlmEvent`] items.
///
/// ## Object safety
/// This trait is object-safe — you can use `Arc<dyn LlmProvider>` in the
/// agent loop. The boxed-future return type (instead of RPITIT `impl Future`)
/// is what makes this possible.
pub trait LlmProvider: Send + Sync {
    /// Start a streaming chat completion.
    ///
    /// Returns a [`LlmStreamFuture`] — a pinned, boxed, sendable future that
    /// resolves to a [`LlmStream`]. Each item in the stream is a single delta:
    /// text tokens, thinking tokens, tool-use fragments, or terminal signals
    /// (`Done`, `Error`).
    fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        config: &RequestConfig,
    ) -> LlmStreamFuture<'_>;
}
