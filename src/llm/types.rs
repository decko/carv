//! LLM wire types — provider-agnostic message, tool, and streaming types.
//!
//! These are the core types used by the `LlmProvider` trait to communicate
//! with Anthropic, OpenAI, or other LLM backends. They are distinct from the
//! output-level types in `crate::stream::output`.

/// Content type within a message block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
        is_error: bool,
    },
}

/// Cache control annotation for prompt caching.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String, // "ephemeral"
}

/// A content block within a message (content + optional cache control).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContentBlock {
    pub content: ContentType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Role of the message sender.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A message in the conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// Create a simple user message with a single text block.
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock {
                content: ContentType::Text { text: text.into() },
                cache_control: None,
            }],
        }
    }

    /// Create a simple system message with a single text block.
    pub fn system(text: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: vec![ContentBlock {
                content: ContentType::Text { text: text.into() },
                cache_control: None,
            }],
        }
    }
}

/// Input schema for a tool parameter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolProperty {
    #[serde(rename = "type")]
    pub prop_type: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#enum: Vec<String>,
}

/// Provider-agnostic tool definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Configuration for a single LLM request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// Event emitted during LLM streaming.
///
/// These are provider-level events, distinct from the output-level
/// `crate::stream::output::StreamEvent`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
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
        usage: Option<Usage>,
    },
    #[serde(rename = "error")]
    Error { error: String },
}

/// Token usage for a single LLM request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_user_roundtrip() {
        let msg = Message::user("hello world");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, Role::User);
        assert_eq!(deserialized.content.len(), 1);
        match &deserialized.content[0].content {
            ContentType::Text { text } => assert_eq!(text, "hello world"),
            other => panic!("expected Text content, got {:?}", other),
        }
    }

    #[test]
    fn test_message_system_roundtrip() {
        let msg = Message::system("system prompt");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, Role::System);
        match &deserialized.content[0].content {
            ContentType::Text { text } => assert_eq!(text, "system prompt"),
            other => panic!("expected Text content, got {:?}", other),
        }
    }

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

    #[test]
    fn test_stream_event_text_roundtrip() {
        let event = StreamEvent::Text {
            text: "some text".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            StreamEvent::Text { text } => assert_eq!(text, "some text"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_text_json_shape() {
        let event = StreamEvent::Text {
            text: "hello".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello");
    }

    #[test]
    fn test_stream_event_tool_use_complete_roundtrip() {
        let event = StreamEvent::ToolUseComplete {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/foo"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            StreamEvent::ToolUseComplete {
                id,
                name,
                ref input,
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "/tmp/foo");
            }
            other => panic!("expected ToolUseComplete, got {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_tool_use_delta_roundtrip() {
        let event = StreamEvent::ToolUseDelta {
            id: "call_1".to_string(),
            name: Some("read_file".to_string()),
            input_json: r#"{"path": "/tm"#.to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            StreamEvent::ToolUseDelta {
                id,
                name,
                input_json,
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, Some("read_file".to_string()));
                assert_eq!(input_json, r#"{"path": "/tm"#);
            }
            other => panic!("expected ToolUseDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_done_roundtrip() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: Some(10),
            cache_creation_tokens: Some(5),
        };
        let event = StreamEvent::Done { usage: Some(usage) };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            StreamEvent::Done { usage } => {
                let u = usage.expect("usage should be present");
                assert_eq!(u.input_tokens, 100);
                assert_eq!(u.output_tokens, 50);
                assert_eq!(u.cache_read_tokens, Some(10));
                assert_eq!(u.cache_creation_tokens, Some(5));
            }
            other => panic!("expected Done, got {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_done_no_usage_roundtrip() {
        let event = StreamEvent::Done { usage: None };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            StreamEvent::Done { usage } => assert!(usage.is_none()),
            other => panic!("expected Done, got {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_error_roundtrip() {
        let event = StreamEvent::Error {
            error: "rate limited".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            StreamEvent::Error { error } => assert_eq!(error, "rate limited"),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn test_stream_event_error_json_shape() {
        let event = StreamEvent::Error {
            error: "something broke".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"], "something broke");
    }

    #[test]
    fn test_usage_all_fields_roundtrip() {
        let usage = Usage {
            input_tokens: 200,
            output_tokens: 150,
            cache_read_tokens: Some(20),
            cache_creation_tokens: Some(10),
        };
        let json = serde_json::to_string(&usage).unwrap();
        let deserialized: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.input_tokens, 200);
        assert_eq!(deserialized.output_tokens, 150);
        assert_eq!(deserialized.cache_read_tokens, Some(20));
        assert_eq!(deserialized.cache_creation_tokens, Some(10));
    }

    #[test]
    fn test_usage_optional_fields_roundtrip() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let deserialized: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.input_tokens, 100);
        assert_eq!(deserialized.output_tokens, 50);
        assert!(deserialized.cache_read_tokens.is_none());
        assert!(deserialized.cache_creation_tokens.is_none());
    }

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
        // Default-false fields should serialize
        assert_eq!(json["thinking"], false);
        // Empty vec should be omitted
        assert!(json.get("stop_sequences").is_none());
        // None fields should be omitted
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

    #[test]
    fn test_role_serialization() {
        assert_eq!(serde_json::to_value(Role::System).unwrap(), "system");
        assert_eq!(serde_json::to_value(Role::User).unwrap(), "user");
        assert_eq!(serde_json::to_value(Role::Assistant).unwrap(), "assistant");
    }

    #[test]
    fn test_content_block_with_cache_control() {
        let block = ContentBlock {
            content: ContentType::Text {
                text: "cached text".to_string(),
            },
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
            }),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["content"]["type"], "text");
        assert_eq!(json["content"]["text"], "cached text");
        assert_eq!(json["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_tool_property_roundtrip() {
        let prop = ToolProperty {
            prop_type: "string".to_string(),
            description: "A file path".to_string(),
            r#enum: vec![],
        };
        let json = serde_json::to_string(&prop).unwrap();
        let deserialized: ToolProperty = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.prop_type, "string");
        assert_eq!(deserialized.description, "A file path");
        assert!(deserialized.r#enum.is_empty());
    }

    #[test]
    fn test_tool_property_with_enum() {
        let prop = ToolProperty {
            prop_type: "string".to_string(),
            description: "Log level".to_string(),
            r#enum: vec!["debug".to_string(), "info".to_string(), "error".to_string()],
        };
        let json = serde_json::to_value(&prop).unwrap();
        assert_eq!(json["type"], "string");
        assert_eq!(json["enum"][0], "debug");
        assert_eq!(json["enum"][1], "info");
        assert_eq!(json["enum"][2], "error");
    }
}
