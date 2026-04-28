//! `LlmProvider` trait — abstract interface for LLM backends.
//!
//! Each provider (Anthropic, OpenAI) implements this trait to expose a
//! unified streaming chat interface. The agent loop calls `stream_chat` and
//! consumes `LlmEvent` items without knowing which backend is in use.
//!
//! ## Design
//! - Uses native async fn in traits (RPITIT, Rust 1.75+) — no `async-trait` crate.
//! - Streams `LlmEvent` items; each provider translates its own SSE deltas
//!   into this unified type.
//! - Errors are returned via `anyhow::Result` at both the stream and item levels.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use futures::Stream;

use crate::llm::types::{LlmEvent, Message, RequestConfig, ToolDef};

/// Abstract LLM provider capable of streaming chat completions with tool use.
///
/// Implementations handle the provider-specific wire protocol (SSE event
/// parsing, authentication, request formatting) and emit a unified stream
/// of [`LlmEvent`] items.
pub trait LlmProvider: Send + Sync {
    /// Start a streaming chat completion.
    ///
    /// Returns a `Future` that resolves to a pinned, boxed, sendable stream of
    /// `Result<LlmEvent>`. Each item in the stream represents a single delta:
    /// text tokens, thinking tokens, tool-use fragments, or terminal signals
    /// (`Done`, `Error`).
    fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        config: &RequestConfig,
    ) -> impl Future<Output = Result<Pin<Box<dyn Stream<Item = Result<LlmEvent>> + Send>>>> + Send;
}
