//! Anthropic `/v1/messages` API — request (Serialize), SSE (Deserialize), tool mapping.
use crate::llm::types::{ContentBlock, Message, ToolDef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AnthropicSystem {
    String(String),
    Blocks(Vec<ContentBlock>),
}
#[derive(Debug, Clone, Serialize)]
pub struct AnthropicThinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    pub budget_tokens: u32,
}
#[derive(Debug, Clone, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicUsageSummary {
    #[serde(default)]
    pub input_tokens: Option<u32>,
    #[serde(default)]
    pub output_tokens: Option<u32>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessageStart {
    pub id: String,
    pub role: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub usage: AnthropicUsageSummary,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
}
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessageDelta {
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}
#[rustfmt::skip]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AnthropicSseEvent {
    #[serde(rename = "message_start")] MessageStart { message: AnthropicMessageStart },
    #[serde(rename = "content_block_start")] ContentBlockStart { index: u32, content_block: ContentBlock },
    #[serde(rename = "content_block_delta")] ContentBlockDelta { index: u32, delta: AnthropicDelta },
    #[serde(rename = "content_block_stop")] ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")] MessageDelta { delta: AnthropicMessageDelta, usage: AnthropicUsageSummary },
    #[serde(rename = "message_stop")] MessageStop {},
    #[serde(rename = "error")] Error { error: AnthropicError },
    #[serde(rename = "ping")] Ping {},
}

pub fn to_anthropic_tools(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools.iter().map(|t| serde_json::json!({"name": t.name, "description": t.description, "input_schema": t.input_schema})).collect()
}
#[rustfmt::skip]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_anthropic_tools_roundtrip() {
        let tool = ToolDef { name: "read_file".into(), description: "Read a file.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        };
        let json = serde_json::to_value(&to_anthropic_tools(&[tool])).unwrap();
        assert_eq!(json[0]["name"], "read_file");
        assert!(json[0].get("function").is_none(), "Anthropic tools are flat");
        assert_eq!(json[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn test_to_anthropic_tools_empty() { assert!(to_anthropic_tools(&[]).is_empty()); }

    #[test]
    fn test_anthropic_request_serialization() {
        let json = serde_json::to_value(&AnthropicRequest {
            model: "claude".into(), max_tokens: 4096, messages: vec![Message::user("hello")],
            system: Some(AnthropicSystem::String("You are Claude.".into())),
            tools: vec![serde_json::json!({"name":"read_file","description":"Read","input_schema":{"type":"object"}})],
            stream: true, thinking: None, temperature: Some(0.5), top_p: None, stop_sequences: vec![],
        }).unwrap();
        assert_eq!(json["model"], "claude");
        assert_eq!(json["stream"], true);
        assert_eq!(json["system"], "You are Claude.");
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn test_system_as_content_blocks() {
        let json = serde_json::to_value(AnthropicSystem::Blocks(vec![ContentBlock::text("hello")])).unwrap();
        assert!(json.is_array());
        assert_eq!(json[0]["type"], "text");
        assert_eq!(json[0]["text"], "hello");
    }

    #[test]
    fn test_sse_event_deserialization() {
        let event: AnthropicSseEvent = serde_json::from_str(r#"{"type":"message_start","message":{"id":"msg_123","role":"assistant","model":"claude","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}"#).unwrap();
        match event { AnthropicSseEvent::MessageStart { message } => {
            assert_eq!(message.id, "msg_123");
            assert_eq!(message.usage.input_tokens, Some(10));
        } other => panic!("expected MessageStart, got {other:?}"), }
    }

    #[test]
    fn test_content_block_delta_deserialization() {
        let event: AnthropicSseEvent = serde_json::from_str(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello world"}}"#).unwrap();
        match event { AnthropicSseEvent::ContentBlockDelta { index, delta } => {
            assert_eq!(index, 1);
            assert_eq!(delta, AnthropicDelta::Text { text: "Hello world".into() });
        } other => panic!("expected ContentBlockDelta, got {other:?}"), }
    }
}
