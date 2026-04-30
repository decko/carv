//! LLM wire types — provider-agnostic message, tool, and streaming types.
//!
//! These are the core types used by the `LlmProvider` trait to communicate
//! with Anthropic, OpenAI, or other LLM backends. They are distinct from the
//! output-level types in `crate::stream::output`.
//!
//! ## Naming
//! Types are prefixed `Llm*` (e.g. `LlmEvent`, `LlmUsage`) to avoid collision
//! with output-level `StreamEvent` and `Usage` in `crate::stream::output`.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use futures::Stream;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Content types
// ---------------------------------------------------------------------------

/// Content type within a message block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentType {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// Anthropic extended thinking content block.
    ///
    /// Present when `thinking.enabled` is set in the request. The initial
    /// `thinking` text arrives via `content_block_start`; subsequent chunks
    /// arrive as `thinking_delta` events.
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: String,
    },
}

/// Cache control annotation for prompt caching (currently only "ephemeral").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String, // "ephemeral"
}

/// A content block within a message.
///
/// The `content` field is flattened into the block so that the wire format
/// matches both Anthropic and OpenAI:
/// `{"type": "text", "text": "hello", "cache_control": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(flatten)]
    pub content: ContentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl ContentBlock {
    /// Create a text-only content block.
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock {
            content: ContentType::Text { text: text.into() },
            cache_control: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// Role of the message sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A message in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// Create a simple user message with a single text block.
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Create a simple system message with a single text block.
    pub fn system(text: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Create a simple assistant message with a single text block.
    pub fn assistant(text: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }
}

// ---------------------------------------------------------------------------
// Tool types
// ---------------------------------------------------------------------------

/// Provider-agnostic tool definition.
///
/// Each provider serializes this to its own wire format:
/// - Anthropic: `{"name", "description", "input_schema"}`
/// - OpenAI: `{"type": "function", "function": {"name", "description", "parameters"}}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Request configuration
// ---------------------------------------------------------------------------

/// Configuration for a single LLM request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestConfig {
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default)]
    pub thinking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
}

// ---------------------------------------------------------------------------
// Streaming events
// ---------------------------------------------------------------------------

/// Event emitted during LLM streaming.
///
/// This is a **higher-level abstraction** over raw SSE events. The provider
/// implementations translate provider-specific streaming deltas into this
/// unified enum. The agent loop consumes these events and converts them to
/// output-level `crate::stream::output::StreamEvent` for formatting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LlmEvent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use_delta")]
    ToolUseDelta {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        input_json: String,
    },
    #[serde(rename = "tool_use_complete")]
    ToolUseComplete {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "done")]
    Done {
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<LlmUsage>,
    },
    #[serde(rename = "error")]
    Error { error: String },
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

/// Token usage for a single LLM request.
///
/// Field names match the Anthropic wire format. When integrating with OpenAI,
/// the provider layer maps OpenAI's field names to these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "cache_read_input_tokens"
    )]
    pub cache_read_tokens: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "cache_creation_input_tokens"
    )]
    pub cache_creation_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Stream type aliases
// ---------------------------------------------------------------------------

/// Sendable, pinned stream of [`LlmEvent`] items.
///
/// Used as the return type of [`LlmProvider::stream_chat`]. Wrapping in `Pin`
/// is required for async iteration, and boxing erases the concrete type so the
/// trait remains object-safe.
pub type LlmStream = Pin<Box<dyn Stream<Item = Result<LlmEvent>> + Send>>;

/// Sendable, pinned future resolving to a [`LlmStream`].
///
/// The `'a` lifetime corresponds to the borrow of the provider's `&self`.
/// Using `dyn Future` (instead of `impl Future`) makes the
/// [`LlmProvider`](crate::llm::provider::LlmProvider) trait object-safe.
pub type LlmStreamFuture<'a> = Pin<Box<dyn Future<Output = Result<LlmStream>> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Message round-trip tests --

    #[test]
    fn test_message_user_roundtrip() {
        let msg = Message::user("hello world");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, Role::User);
        assert_eq!(deserialized.content.len(), 1);
        assert_eq!(
            deserialized.content[0].content,
            ContentType::Text {
                text: "hello world".to_string()
            }
        );
    }

    #[test]
    fn test_message_system_roundtrip() {
        let msg = Message::system("system prompt");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, Role::System);
        assert_eq!(
            deserialized.content[0].content,
            ContentType::Text {
                text: "system prompt".to_string()
            }
        );
    }

    #[test]
    fn test_message_assistant_roundtrip() {
        let msg = Message::assistant("I'll do that.");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, Role::Assistant);
        assert_eq!(
            deserialized.content[0].content,
            ContentType::Text {
                text: "I'll do that.".to_string()
            }
        );
    }

    // -- ContentBlock tests --

    #[test]
    fn test_content_block_flattens_to_wire_format() {
        // The Anthropic wire format is flat: {"type": "text", "text": "hello", "cache_control": ...}
        // ContentBlock should NOT nest content under a "content" key.
        let block = ContentBlock {
            content: ContentType::Text {
                text: "hello".to_string(),
            },
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
            }),
        };
        let json = serde_json::to_value(&block).unwrap();

        // Flattened fields at top level
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello");
        assert_eq!(json["cache_control"]["type"], "ephemeral");

        // Should NOT have a nested "content" key
        assert!(json.get("content").is_none());
    }

    #[test]
    fn test_content_block_without_cache_control() {
        let block = ContentBlock::text("plain text");
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "plain text");
        assert!(json.get("cache_control").is_none());
    }

    // -- ContentType tests --

    #[test]
    fn test_content_type_text_json_shape() {
        let ct = ContentType::Text {
            text: "hello".to_string(),
        };
        let json = serde_json::to_value(&ct).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello");
    }

    #[test]
    fn test_content_type_tool_use_json_shape() {
        let ct = ContentType::ToolUse {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/foo"}),
        };
        let json = serde_json::to_value(&ct).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "call_1");
        assert_eq!(json["name"], "read_file");
        assert_eq!(json["input"]["path"], "/tmp/foo");
    }

    #[test]
    fn test_content_type_tool_result_json_shape() {
        let ct = ContentType::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: "file contents".to_string(),
            is_error: false,
        };
        let json = serde_json::to_value(&ct).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "call_1");
        assert_eq!(json["content"], "file contents");
        assert_eq!(json["is_error"], false);
    }

    #[test]
    fn test_content_type_tool_result_deserializes_without_is_error() {
        // The API omits is_error on successful tool results. Without
        // #[serde(default)], deserialization would fail here.
        let json = r#"{"type":"tool_result","tool_use_id":"call_1","content":"done"}"#;
        let ct: ContentType = serde_json::from_str(json).unwrap();
        assert_eq!(
            ct,
            ContentType::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "done".to_string(),
                is_error: false,
            }
        );
    }

    #[test]
    fn test_content_type_thinking_roundtrip() {
        let ct = ContentType::Thinking {
            thinking: "Let me reason about this.".to_string(),
            signature: "sig_abc123".to_string(),
        };
        let json = serde_json::to_value(&ct).unwrap();
        assert_eq!(json["type"], "thinking");
        assert_eq!(json["thinking"], "Let me reason about this.");
        assert_eq!(json["signature"], "sig_abc123");

        // Round-trip: serialize then deserialize
        let deserialized: ContentType = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, ct);
    }

    #[test]
    fn test_content_type_thinking_no_signature() {
        // signature has #[serde(default)], so it can be absent
        let json = r#"{"type":"thinking","thinking":"A quick thought"}"#;
        let ct: ContentType = serde_json::from_str(json).unwrap();
        assert_eq!(
            ct,
            ContentType::Thinking {
                thinking: "A quick thought".to_string(),
                signature: String::new(),
            }
        );
    }

    // -- ToolDef tests --

    #[test]
    fn test_tool_def_roundtrip() {
        let tool = ToolDef {
            name: "read_file".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to file"
                    }
                }
            }),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let deserialized: ToolDef = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "read_file");
        assert_eq!(deserialized.description, "Read a file from disk");
        assert_eq!(deserialized.input_schema["type"], "object");
    }

    // -- LlmEvent tests --

    #[test]
    fn test_llm_event_text_roundtrip() {
        let event = LlmEvent::Text {
            text: "some text".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized,
            LlmEvent::Text {
                text: "some text".to_string()
            }
        );
    }

    #[test]
    fn test_llm_event_text_json_shape() {
        let event = LlmEvent::Text {
            text: "hello".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello");
    }

    #[test]
    fn test_llm_event_tool_use_complete_roundtrip() {
        let event = LlmEvent::ToolUseComplete {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/foo"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized,
            LlmEvent::ToolUseComplete {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "/tmp/foo"}),
            }
        );
    }

    #[test]
    fn test_llm_event_tool_use_delta_roundtrip() {
        let event = LlmEvent::ToolUseDelta {
            id: "call_1".to_string(),
            name: Some("read_file".to_string()),
            input_json: r#"{"path": "/tm"#.to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized,
            LlmEvent::ToolUseDelta {
                id: "call_1".to_string(),
                name: Some("read_file".to_string()),
                input_json: r#"{"path": "/tm"#.to_string(),
            }
        );
    }

    #[test]
    fn test_llm_event_done_roundtrip() {
        let usage = LlmUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: Some(10),
            cache_creation_tokens: Some(5),
        };
        let event = LlmEvent::Done {
            usage: Some(usage.clone()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, LlmEvent::Done { usage: Some(usage) });
    }

    #[test]
    fn test_llm_event_done_no_usage_roundtrip() {
        let event = LlmEvent::Done { usage: None };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, LlmEvent::Done { usage: None });
    }

    #[test]
    fn test_llm_event_error_roundtrip() {
        let event = LlmEvent::Error {
            error: "rate limited".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized,
            LlmEvent::Error {
                error: "rate limited".to_string()
            }
        );
    }

    #[test]
    fn test_llm_event_error_json_shape() {
        let event = LlmEvent::Error {
            error: "something broke".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"], "something broke");
    }

    // -- LlmUsage tests --

    #[test]
    fn test_llm_usage_all_fields_roundtrip() {
        let usage = LlmUsage {
            input_tokens: 200,
            output_tokens: 150,
            cache_read_tokens: Some(20),
            cache_creation_tokens: Some(10),
        };
        let json = serde_json::to_string(&usage).unwrap();
        let deserialized: LlmUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.input_tokens, 200);
        assert_eq!(deserialized.output_tokens, 150);
        assert_eq!(deserialized.cache_read_tokens, Some(20));
        assert_eq!(deserialized.cache_creation_tokens, Some(10));
    }

    #[test]
    fn test_llm_usage_optional_fields_roundtrip() {
        let usage = LlmUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let deserialized: LlmUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.input_tokens, 100);
        assert_eq!(deserialized.output_tokens, 50);
        assert!(deserialized.cache_read_tokens.is_none());
        assert!(deserialized.cache_creation_tokens.is_none());
    }

    #[test]
    fn test_llm_usage_deserializes_anthropic_field_names() {
        // Anthropic sends cache_read_input_tokens / cache_creation_input_tokens.
        // Our struct uses default field names, so we alias the Anthropic names.
        let json = r#"{
            "input_tokens": 500,
            "output_tokens": 200,
            "cache_read_input_tokens": 300,
            "cache_creation_input_tokens": 100
        }"#;
        let usage: LlmUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.cache_read_tokens, Some(300));
        assert_eq!(usage.cache_creation_tokens, Some(100));
    }

    // -- RequestConfig tests --

    #[test]
    fn test_request_config_defaults() {
        let config = RequestConfig {
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            stop_sequences: vec![],
            thinking: false,
            thinking_budget: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["thinking"], false);
        assert!(json.get("stop_sequences").is_none());
        assert!(json.get("temperature").is_none());
        assert!(json.get("top_p").is_none());
        assert!(json.get("thinking_budget").is_none());
    }

    #[test]
    fn test_request_config_all_fields() {
        let config = RequestConfig {
            max_tokens: 8192,
            temperature: Some(0.7),
            top_p: Some(0.9),
            stop_sequences: vec!["\n\n".to_string()],
            thinking: true,
            thinking_budget: Some(1024),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["max_tokens"], 8192);
        assert!((json["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
        assert!((json["top_p"].as_f64().unwrap() - 0.9).abs() < 1e-6);
        assert_eq!(json["stop_sequences"][0], "\n\n");
        assert_eq!(json["thinking"], true);
        assert_eq!(json["thinking_budget"], 1024);
    }

    // -- Role tests --

    #[test]
    fn test_role_serialization() {
        assert_eq!(serde_json::to_value(Role::System).unwrap(), "system");
        assert_eq!(serde_json::to_value(Role::User).unwrap(), "user");
        assert_eq!(serde_json::to_value(Role::Assistant).unwrap(), "assistant");
    }

    // -- PartialEq tests --

    #[test]
    fn test_content_type_partial_eq() {
        let a = ContentType::Text {
            text: "hello".to_string(),
        };
        let b = ContentType::Text {
            text: "hello".to_string(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_llm_event_partial_eq() {
        let a = LlmEvent::Text {
            text: "hello".to_string(),
        };
        let b = LlmEvent::Text {
            text: "hello".to_string(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_content_block_convenience() {
        let block = ContentBlock::text("hello");
        assert_eq!(
            block.content,
            ContentType::Text {
                text: "hello".to_string()
            }
        );
        assert!(block.cache_control.is_none());
    }
}
