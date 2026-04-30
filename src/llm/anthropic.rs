//! Anthropic `/v1/messages` API — request (Serialize), SSE (Deserialize), tool mapping.
use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use reqwest_eventsource::{retry::RetryPolicy, Event, EventSource};

use crate::llm::provider::LlmProvider;
use crate::llm::types::{
    ContentBlock, ContentType, LlmEvent, LlmStream, LlmStreamFuture, LlmUsage, Message,
    RequestConfig, Role, ToolDef,
};
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
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
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
// Keep each variant on one line for readability against the Anthropic SSE spec
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

// ---------------------------------------------------------------------------
// Retry policy — we handle retry in the agent loop, not at the SSE layer
// ---------------------------------------------------------------------------

/// Retry policy that never retries. The agent loop handles retry with
/// exponential backoff; reconnecting at the SSE layer would create
/// duplicate requests.
struct NoRetry;

impl RetryPolicy for NoRetry {
    fn retry(
        &self,
        _error: &reqwest_eventsource::Error,
        _last: Option<(usize, Duration)>,
    ) -> Option<Duration> {
        None
    }

    fn set_reconnection_time(&mut self, _duration: Duration) {}
}

// ---------------------------------------------------------------------------
// SSE → LlmEvent state machine types
// ---------------------------------------------------------------------------

/// Per-block tracking during SSE streaming.
///
/// Anthropic content blocks arrive in three phases: `content_block_start`
/// (type + metadata), `content_block_delta` (payload chunks), and
/// `content_block_stop` (block finished). For `tool_use` blocks, partial
/// JSON fragments are accumulated and then parsed into the complete
/// `serde_json::Value` on `content_block_stop`.
#[derive(Debug)]
enum BlockState {
    Text,
    Thinking,
    ToolUse {
        id: String,
        name: String,
        json_acc: String,
    },
}

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

static ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic `/v1/messages` SSE client implementing [`LlmProvider`].
///
/// Streams chat completions with tool use, prompt caching, and extended
/// thinking support. Translates Anthropic's SSE wire protocol into the
/// unified [`LlmEvent`] stream consumed by the agent loop.
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::Client::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the system prompt from the messages list for Anthropic's
/// top-level `system` field.
///
/// Anthropic places the system prompt in a separate `system` field on the
/// request, not in the `messages` array. This helper locates the first
/// `Role::System` message, removes it, and converts it to the wire format.
fn extract_system(mut messages: Vec<Message>) -> (Option<AnthropicSystem>, Vec<Message>) {
    if let Some(idx) = messages.iter().position(|m| m.role == Role::System) {
        let system_msg = messages.remove(idx);
        // Use String variant for single text block, Blocks for everything else
        let system = system_msg.content;
        if system.len() == 1 {
            if let ContentType::Text { text } = &system[0].content {
                return (Some(AnthropicSystem::String(text.clone())), messages);
            }
        }
        (Some(AnthropicSystem::Blocks(system)), messages)
    } else {
        (None, messages)
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

impl LlmProvider for AnthropicProvider {
    fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        config: &RequestConfig,
    ) -> LlmStreamFuture<'_> {
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let client = self.client.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let config = config.clone();

        Box::pin(async move {
            // Separate system prompt for Anthropic top-level `system` field
            let (system, messages) = extract_system(messages);

            // Build extended thinking config if enabled
            let thinking = if config.thinking {
                Some(AnthropicThinking {
                    thinking_type: "enabled".to_string(),
                    budget_tokens: config.thinking_budget.unwrap_or(1024),
                })
            } else {
                None
            };

            let request = AnthropicRequest {
                model,
                max_tokens: config.max_tokens,
                messages,
                system,
                tools: to_anthropic_tools(&tools),
                stream: true,
                thinking,
                temperature: config.temperature,
                top_p: config.top_p,
                stop_sequences: config.stop_sequences.clone(),
            };

            // Build the HTTP request
            let builder = client
                .post(format!("{}/v1/messages", ANTHROPIC_BASE_URL))
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&request);

            // EventSource adds Accept: text/event-stream and handles SSE parsing
            let mut es = EventSource::new(builder)
                .map_err(|e| anyhow::anyhow!("Failed to create SSE event source: {e:?}"))?;
            es.set_retry_policy(Box::new(NoRetry));

            // State machine: SSE events → LlmEvent stream
            //
            // We use futures::stream::unfold to drive the EventSource and
            // map raw SSE events into LlmEvent items.  Intermediate events
            // (like `content_block_start` that just record metadata, or
            // `ping`) loop without yielding until a user-visible event is
            // ready.
            use futures::stream;

            let llm_stream = stream::unfold(
                (
                    es,
                    HashMap::<u32, BlockState>::new(),
                    None,  // input_tokens
                    None,  // output_tokens
                    None,  // cache_read
                    None,  // cache_creation
                    false, // done
                ),
                move |(
                    mut es,
                    mut blocks,
                    mut input_tok,
                    mut output_tok,
                    mut cache_read,
                    mut cache_creation,
                    mut done,
                )| async move {
                    if done {
                        return None;
                    }

                    loop {
                        let item = match es.next().await {
                            Some(event) => event,
                            None => {
                                // Stream ended without message_stop — synthesize Done
                                let usage =
                                    make_usage(input_tok, output_tok, cache_read, cache_creation);
                                done = true;
                                return Some((
                                    Ok(LlmEvent::Done { usage }),
                                    (
                                        es,
                                        blocks,
                                        input_tok,
                                        output_tok,
                                        cache_read,
                                        cache_creation,
                                        done,
                                    ),
                                ));
                            }
                        };

                        let msg = match item {
                            Ok(Event::Open) => continue, // connection opened, no payload
                            Ok(Event::Message(msg)) => msg,
                            Err(e) => {
                                done = true;
                                return Some((
                                    Err(anyhow::anyhow!("SSE transport error: {e}")),
                                    (
                                        es,
                                        blocks,
                                        input_tok,
                                        output_tok,
                                        cache_read,
                                        cache_creation,
                                        done,
                                    ),
                                ));
                            }
                        };

                        let event: AnthropicSseEvent = match serde_json::from_str(&msg.data) {
                            Ok(ev) => ev,
                            Err(e) => {
                                // Unknown or malformed event — log and skip
                                tracing::warn!(
                                    event_type = %msg.event,
                                    error = %e,
                                    "Skipping unparseable SSE event"
                                );
                                continue;
                            }
                        };

                        match event {
                            AnthropicSseEvent::MessageStart { message } => {
                                input_tok = message.usage.input_tokens;
                                cache_read = message.usage.cache_read_input_tokens;
                                cache_creation = message.usage.cache_creation_input_tokens;
                                // continue looping — nothing to emit yet
                            }
                            AnthropicSseEvent::ContentBlockStart {
                                index,
                                content_block,
                            } => {
                                let state = match &content_block.content {
                                    ContentType::Text { .. } => BlockState::Text,
                                    ContentType::Thinking { .. } => BlockState::Thinking,
                                    ContentType::ToolUse { id, name, .. } => BlockState::ToolUse {
                                        id: id.clone(),
                                        name: name.clone(),
                                        json_acc: String::new(),
                                    },
                                    ContentType::ToolResult { .. } => BlockState::Text,
                                };
                                blocks.insert(index, state);
                            }
                            AnthropicSseEvent::ContentBlockDelta { index, delta } => {
                                match delta {
                                    AnthropicDelta::Text { text } => {
                                        return Some((
                                            Ok(LlmEvent::Text { text }),
                                            (
                                                es,
                                                blocks,
                                                input_tok,
                                                output_tok,
                                                cache_read,
                                                cache_creation,
                                                done,
                                            ),
                                        ));
                                    }
                                    AnthropicDelta::Thinking { thinking } => {
                                        return Some((
                                            Ok(LlmEvent::Thinking { thinking }),
                                            (
                                                es,
                                                blocks,
                                                input_tok,
                                                output_tok,
                                                cache_read,
                                                cache_creation,
                                                done,
                                            ),
                                        ));
                                    }
                                    AnthropicDelta::InputJson { partial_json } => {
                                        let key = index;
                                        if let Some(BlockState::ToolUse {
                                            id,
                                            name,
                                            ref mut json_acc,
                                        }) = blocks.get_mut(&key)
                                        {
                                            json_acc.push_str(&partial_json);
                                            return Some((
                                                Ok(LlmEvent::ToolUseDelta {
                                                    id: id.clone(),
                                                    name: Some(name.clone()),
                                                    input_json: partial_json,
                                                }),
                                                (
                                                    es,
                                                    blocks,
                                                    input_tok,
                                                    output_tok,
                                                    cache_read,
                                                    cache_creation,
                                                    done,
                                                ),
                                            ));
                                        }
                                        // ToolUseDelta for unknown index — ignore
                                    }
                                    AnthropicDelta::Signature { .. } => {
                                        // Signature deltas accumulate internally;
                                        // the agent loop doesn't need them.
                                    }
                                }
                            }
                            AnthropicSseEvent::ContentBlockStop { index } => {
                                if let Some(BlockState::ToolUse { id, name, json_acc }) =
                                    blocks.remove(&index)
                                {
                                    match serde_json::from_str(&json_acc) {
                                        Ok(input) => {
                                            return Some((
                                                Ok(LlmEvent::ToolUseComplete { id, name, input }),
                                                (
                                                    es,
                                                    blocks,
                                                    input_tok,
                                                    output_tok,
                                                    cache_read,
                                                    cache_creation,
                                                    done,
                                                ),
                                            ));
                                        }
                                        Err(e) => {
                                            return Some((
                                                Ok(LlmEvent::Error {
                                                    error: format!(
                                                        "Failed to parse tool input JSON: {e}"
                                                    ),
                                                }),
                                                (
                                                    es,
                                                    blocks,
                                                    input_tok,
                                                    output_tok,
                                                    cache_read,
                                                    cache_creation,
                                                    done,
                                                ),
                                            ));
                                        }
                                    }
                                }
                            }
                            AnthropicSseEvent::MessageDelta {
                                usage: delta_usage, ..
                            } => {
                                // message_delta carries the final output token count
                                output_tok = delta_usage.output_tokens;
                            }
                            AnthropicSseEvent::MessageStop {} => {
                                let usage =
                                    make_usage(input_tok, output_tok, cache_read, cache_creation);
                                done = true;
                                return Some((
                                    Ok(LlmEvent::Done { usage }),
                                    (
                                        es,
                                        blocks,
                                        input_tok,
                                        output_tok,
                                        cache_read,
                                        cache_creation,
                                        done,
                                    ),
                                ));
                            }
                            AnthropicSseEvent::Error { error } => {
                                done = true;
                                return Some((
                                    Ok(LlmEvent::Error {
                                        error: format!("{}: {}", error.error_type, error.message),
                                    }),
                                    (
                                        es,
                                        blocks,
                                        input_tok,
                                        output_tok,
                                        cache_read,
                                        cache_creation,
                                        done,
                                    ),
                                ));
                            }
                            AnthropicSseEvent::Ping {} => {
                                // Keep-alive pings — ignore
                            }
                        }
                        // If we reach here, the event didn't produce a stream item;
                        // continue the inner loop.
                    }
                },
            );

            Ok(Box::pin(llm_stream) as LlmStream)
        })
    }
}

/// Synthesize an [`LlmUsage`] from the fields accumulated across
/// `message_start` and `message_delta`.
fn make_usage(
    input: Option<u32>,
    output: Option<u32>,
    cache_read: Option<u32>,
    cache_create: Option<u32>,
) -> Option<LlmUsage> {
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(LlmUsage {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_create,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Keep test definitions compact and scannable against the SSE spec
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

    #[test]
    fn test_content_block_start_deserialization() {
        // ContentBlockStart for a thinking block with signature
        let json = r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Let me think about this","signature":"sig_abc"}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        match event { AnthropicSseEvent::ContentBlockStart { index, content_block } => {
            assert_eq!(index, 0);
            assert_eq!(content_block.content, ContentType::Thinking { thinking: "Let me think about this".into(), signature: "sig_abc".into() });
        } other => panic!("expected ContentBlockStart, got {other:?}"), }
    }

    #[test]
    fn test_content_block_stop_deserialization() {
        let event: AnthropicSseEvent = serde_json::from_str(r#"{"type":"content_block_stop","index":2}"#).unwrap();
        match event { AnthropicSseEvent::ContentBlockStop { index } => {
            assert_eq!(index, 2);
        } other => panic!("expected ContentBlockStop, got {other:?}"), }
    }

    #[test]
    fn test_message_delta_deserialization() {
        // message_delta carries usage at the top level alongside delta,
        // not nested inside delta — this is the trickiest wire format.
        let json = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":42}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        match event { AnthropicSseEvent::MessageDelta { delta, usage } => {
            assert_eq!(delta.stop_reason, Some("end_turn".into()));
            assert!(delta.stop_sequence.is_none());
            assert_eq!(usage.output_tokens, Some(42));
            assert!(usage.input_tokens.is_none());
        } other => panic!("expected MessageDelta, got {other:?}"), }
    }

    #[test]
    fn test_message_stop_deserialization() {
        let event: AnthropicSseEvent = serde_json::from_str(r#"{"type":"message_stop"}"#).unwrap();
        match event { AnthropicSseEvent::MessageStop {} => {},
            other => panic!("expected MessageStop, got {other:?}"), }
    }

    #[test]
    fn test_error_sse_event_deserialization() {
        let json = r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#;
        let event: AnthropicSseEvent = serde_json::from_str(json).unwrap();
        match event { AnthropicSseEvent::Error { error } => {
            assert_eq!(error.error_type, "api_error");
            assert_eq!(error.message, "Internal server error");
        } other => panic!("expected Error, got {other:?}"), }
    }

    #[test]
    fn test_ping_sse_event_deserialization() {
        let event: AnthropicSseEvent = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        match event { AnthropicSseEvent::Ping {} => {},
            other => panic!("expected Ping, got {other:?}"), }
    }

    // -- extract_system tests --

    #[test]
    fn test_extract_system_single_text() {
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("hello"),
        ];
        let (system, rest) = extract_system(messages);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].role, Role::User);
        match system {
            Some(AnthropicSystem::String(s)) => {
                assert_eq!(s, "You are a helpful assistant.");
            }
            other => panic!("expected AnthropicSystem::String, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_system_no_system() {
        let messages = vec![
            Message::user("hello"),
            Message::assistant("hi"),
        ];
        let (system, rest) = extract_system(messages);
        assert!(system.is_none());
        assert_eq!(rest.len(), 2);
    }

    #[test]
    fn test_extract_system_multi_block() {
        let mut system_msg = Message::system("hello");
        system_msg.content.push(ContentBlock::text("world"));
        let messages = vec![system_msg, Message::user("q")];
        let (system, rest) = extract_system(messages);
        assert_eq!(rest.len(), 1);
        match system {
            Some(AnthropicSystem::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 2);
            }
            other => panic!("expected AnthropicSystem::Blocks, got {other:?}"),
        }
    }

    // -- make_usage tests --

    #[test]
    fn test_make_usage_all_fields() {
        let usage = make_usage(Some(100), Some(50), Some(20), Some(10));
        let u = usage.unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_tokens, Some(20));
        assert_eq!(u.cache_creation_tokens, Some(10));
    }

    #[test]
    fn test_make_usage_no_data() {
        assert!(make_usage(None, None, None, None).is_none());
    }

    #[test]
    fn test_make_usage_partial() {
        let usage = make_usage(Some(50), None, None, None).unwrap();
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 0);
    }

    // -- AnthropicUsageSummary cache fields --

    #[test]
    fn test_usage_summary_with_cache_fields() {
        let json = r#"{
            "input_tokens": 500,
            "output_tokens": 0,
            "cache_read_input_tokens": 300,
            "cache_creation_input_tokens": 100
        }"#;
        let usage: AnthropicUsageSummary = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, Some(500));
        assert_eq!(usage.output_tokens, Some(0));
        assert_eq!(usage.cache_read_input_tokens, Some(300));
        assert_eq!(usage.cache_creation_input_tokens, Some(100));
    }

    #[test]
    fn test_usage_summary_without_cache_fields() {
        let json = r#"{"input_tokens": 100, "output_tokens": 0}"#;
        let usage: AnthropicUsageSummary = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert!(usage.cache_read_input_tokens.is_none());
        assert!(usage.cache_creation_input_tokens.is_none());
    }
}
