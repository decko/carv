//! OpenAI `/v1/chat/completions` — request (Serialize), SSE (Deserialize), tool mapping.
//!
//! OpenAI's streaming API sends SSE events with `data: [DONE]` for completion
//! and `data: {"object": "chat.completion.chunk", ...}` for content chunks.
//! Unlike Anthropic, OpenAI delivers tool call deltas as `function.arguments`
//! string chunks and includes usage data on the final chunk.

#[allow(unused_imports)]
use crate::llm::types::ToolDef;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request types (Serialize)
// ---------------------------------------------------------------------------

/// Request sent to OpenAI's `/v1/chat/completions` endpoint.
///
/// Unlike Anthropic, the system prompt goes inside the `messages` array
/// as a message with `role: "system"`, not as a top-level field.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct OpenAIRequest {
    pub model: String,
    pub messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

/// A message in the OpenAI chat format.
///
/// `role` is one of "system", "user", "assistant", or "tool".
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

/// A tool call from the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: OpenAIFunctionCall,
}

/// The function details within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIFunctionCall {
    pub name: String,
    pub arguments: String,
}

// ---------------------------------------------------------------------------
// SSE event types (Deserialize)
// ---------------------------------------------------------------------------

/// A chunk from the OpenAI streaming SSE response.
///
/// OpenAI sends `data: {"object": "chat.completion.chunk", ...}` events.
/// The final chunk may contain `usage` with token counts.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAISseChunk {
    pub id: Option<String>,
    pub object: String,
    pub created: Option<u64>,
    pub model: Option<String>,
    pub choices: Vec<OpenAIChoice>,
    #[serde(default)]
    pub usage: Option<OpenAIUsage>,
}

/// A single choice within an SSE chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIChoice {
    pub index: u32,
    pub delta: OpenAIDelta,
    pub finish_reason: Option<String>,
}

/// The delta content within a choice.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

/// A tool call delta within an SSE chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: Option<OpenAIFunctionDelta>,
}

/// The function delta (name or arguments) within a tool call delta.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIFunctionDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Token usage information, present on the final chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Tool mapping
// ---------------------------------------------------------------------------

/// Convert provider-agnostic [`ToolDef`]s to OpenAI's tool wire format.
///
/// OpenAI nests the function definition under a `function` key and wraps it
/// with a `type: "function"` marker:
/// ```json
/// {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}
/// ```
pub fn to_openai_tools(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_openai_tools_single() {
        let tool = ToolDef {
            name: "read_file".into(),
            description: "Read a file from disk".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to file",
                    },
                },
                "required": ["path"],
            }),
        };
        let json = to_openai_tools(&[tool]);
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "read_file");
        assert_eq!(json[0]["function"]["description"], "Read a file from disk");
        assert_eq!(json[0]["function"]["parameters"]["type"], "object");
        assert_eq!(json[0]["function"]["parameters"]["required"][0], "path");
    }

    #[test]
    fn test_to_openai_tools_multiple() {
        let tools = vec![
            ToolDef {
                name: "tool_a".into(),
                description: "First tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            ToolDef {
                name: "tool_b".into(),
                description: "Second tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
        ];
        let json = to_openai_tools(&tools);
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "tool_a");
        assert_eq!(json[1]["function"]["name"], "tool_b");
    }

    #[test]
    fn test_to_openai_tools_empty() {
        assert!(to_openai_tools(&[]).is_empty());
    }

    #[test]
    fn test_to_openai_tools_roundtrip_format() {
        let tools = [ToolDef {
            name: "search".into(),
            description: "Search the codebase".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                },
                "required": ["query"],
            }),
        }];
        let json = serde_json::to_value(&to_openai_tools(&tools)).unwrap();
        // Verify nested OpenAI function format
        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "search");
        assert_eq!(json[0]["function"]["parameters"]["type"], "object");
        // OpenAI uses "parameters" not "input_schema"
        assert!(json[0]["function"].get("parameters").is_some());
        assert!(
            json[0].get("input_schema").is_none(),
            "OpenAI format must not have top-level input_schema"
        );
        assert!(
            json[0].get("cache_control").is_none(),
            "OpenAI format does not use cache_control"
        );
    }

    #[test]
    fn test_openai_request_serialization() {
        let req = OpenAIRequest {
            model: "gpt-4".into(),
            messages: vec![OpenAIMessage {
                role: "system".into(),
                content: Some("You are helpful.".into()),
                tool_calls: None,
            }],
            tools: vec![],
            stream: true,
            temperature: Some(0.7),
            top_p: None,
            max_completion_tokens: None,
            stop: vec![],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "gpt-4");
        assert_eq!(json["stream"], true);
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][0]["content"], "You are helpful.");
        assert!(json.get("top_p").is_none());
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn test_openai_sse_chunk_deserialization() {
        let json = r#"{
            "id": "chatcmpl_abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "Hello"},
                "finish_reason": null
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.id.as_deref(), Some("chatcmpl_abc123"));
        assert_eq!(chunk.object, "chat.completion.chunk");
        assert_eq!(chunk.choices.len(), 1);
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
        assert!(chunk.choices[0].finish_reason.is_none());
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_openai_sse_chunk_with_usage() {
        let json = r#"{
            "id": "chatcmpl_xyz",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 30,
                "total_tokens": 80
            }
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(50));
        assert_eq!(usage.completion_tokens, Some(30));
        assert_eq!(usage.total_tokens, Some(80));
    }

    #[test]
    fn test_openai_sse_chunk_minimal_fields() {
        let json = r#"{
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {}
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        assert!(chunk.id.is_none());
        assert!(chunk.created.is_none());
        assert!(chunk.model.is_none());
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_openai_tool_call_delta_deserialization() {
        let json = r#"{
            "index": 0,
            "id": "call_abc",
            "type": "function",
            "function": {
                "name": "read_file",
                "arguments": "{\"path\": \""
            }
        }"#;
        let delta: OpenAIToolCallDelta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.index, 0);
        assert_eq!(delta.id.as_deref(), Some("call_abc"));
        assert_eq!(delta.call_type.as_deref(), Some("function"));
        let func = delta.function.unwrap();
        assert_eq!(func.name.as_deref(), Some("read_file"));
        assert_eq!(func.arguments.as_deref(), Some(r#"{"path": ""#));
    }
}
