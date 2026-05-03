//! OpenAI `/v1/chat/completions` — request (Serialize), SSE (Deserialize), tool mapping.
//!
//! OpenAI's streaming API sends SSE events with `data: [DONE]` for completion
//! and `data: {"object": "chat.completion.chunk", ...}` for content chunks.
//! Unlike Anthropic, OpenAI delivers tool call deltas as `function.arguments`
//! string chunks and includes usage data on the final chunk.

use std::collections::HashMap;

use futures::{stream, StreamExt};
use reqwest_eventsource::{Event, EventSource};

use crate::llm::provider::LlmProvider;
use crate::llm::retry::{backoff_duration, is_retryable, NoRetry, MAX_RETRIES};
use crate::llm::types::{
    LlmEvent, LlmStream, LlmStreamFuture, LlmUsage, Message, RequestConfig, Role, ToolDef,
};
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
///
/// `reasoning_content` is present for o-series models (o1, o3-mini) that
/// emit chain-of-thought tokens before the final response content.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct OpenAIDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
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
    pub name: Option<String>,
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
// OpenAI SSE streaming provider
// ---------------------------------------------------------------------------

const OPENAI_BASE_URL: &str = "https://api.openai.com";

/// Per-tool accumulator during SSE streaming.
///
/// OpenAI streams tool call deltas by index: the first delta carries `id`
/// and `function.name`; subsequent deltas carry `function.arguments`
/// fragments.  On `finish_reason == "tool_calls"`, all accumulated
/// argument JSON is parsed into the final `serde_json::Value`.
#[derive(Debug)]
struct ToolAcc {
    id: String,
    name: Option<String>,
    json_acc: String,
}

/// Accumulated state for the OpenAI `stream::unfold` SSE state machine.
///
/// Named struct (not anonymous tuple) so adding a field touches only the
/// initialisation site and the one place it is updated, not every
/// `return Some(...)` site.  EventSource doesn't implement Debug.
///
/// ## Retry and partial tool accumulation
///
/// When a retry is triggered after a transient HTTP error (429/529), all
/// in-progress tool accumulations (`tool_calls`) and pending completions
/// are cleared.  If a [`LlmEvent::ToolUseDelta`] was already yielded to the
/// consumer before the retry, the final [`LlmEvent::ToolUseComplete`] will
/// never arrive.  In practice this is acceptable: retries only fire on
/// 429/529 errors (not mid-stream), and `MAX_RETRIES` limits exposure to
/// three attempts.  The same limitation exists in the Anthropic provider.
struct OpenAIStreamState {
    es: EventSource,
    /// Tool call accumulation keyed by the tool's `index` in choices.
    tool_calls: HashMap<u32, ToolAcc>,
    /// Completed tool calls waiting to be emitted (drained from `tool_calls`)
    /// so each [`LlmEvent::ToolUseComplete`] can be yielded individually.
    pending_completions: Vec<ToolAcc>,
    /// Usage from the final chunk (carried in `finish_reason == "stop"`).
    last_usage: Option<OpenAIUsage>,
    done: bool,
    /// Cached request info for rebuilding the EventSource on retry.
    api_key: String,
    request: OpenAIRequest,
    retry_count: u32,
}

/// OpenAI `/v1/chat/completions` SSE client implementing [`LlmProvider`].
///
/// Streams chat completions with tool use and reasoning support (o-series
/// models).  Translates OpenAI's SSE protocol into the unified
/// [`LlmEvent`] stream consumed by the agent loop.
pub struct OpenAIProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAIProvider {
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

/// Convert provider-agnostic [`Message`]s to OpenAI's wire format.
///
/// Unlike Anthropic, OpenAI places the system prompt inside the `messages`
/// array (as `role: "system"`), not in a separate top-level field.
///
/// TODO(#22): Support tool result messages (`role: "tool"`) and assistant
/// messages with tool calls. The current implementation only handles text
/// content blocks. Multi-turn agent conversations with tool use will need
/// proper `tool_calls` field population and `role: "tool"` messages for
/// tool results.
fn messages_to_openai(messages: &[Message]) -> Vec<OpenAIMessage> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            }
            .to_string();
            let text = m
                .content
                .iter()
                .filter_map(|b| match &b.content {
                    crate::llm::types::ContentType::Text { text } => Some(text.as_str()),
                    other => {
                        tracing::warn!(
                            content_type = ?other,
                            "messages_to_openai: dropping non-text content block \
                             (tool messages not yet supported — see #22)"
                        );
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            OpenAIMessage {
                role,
                content: if text.is_empty() { None } else { Some(text) },
                tool_calls: None,
            }
        })
        .collect()
}

/// Convert OpenAI's [`OpenAIUsage`] to the unified [`LlmUsage`].
fn make_llm_usage(usage: &OpenAIUsage) -> LlmUsage {
    LlmUsage {
        input_tokens: usage.prompt_tokens.unwrap_or(0),
        output_tokens: usage.completion_tokens.unwrap_or(0),
        cache_read_tokens: None,
        cache_creation_tokens: None,
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

impl LlmProvider for OpenAIProvider {
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
            let openai_messages = messages_to_openai(&messages);

            let request = OpenAIRequest {
                model,
                messages: openai_messages,
                tools: to_openai_tools(&tools),
                stream: true,
                temperature: config.temperature,
                top_p: config.top_p,
                max_completion_tokens: Some(config.max_tokens),
                stop: config.stop_sequences.clone(),
            };

            let builder = client
                .post(format!("{}/v1/chat/completions", OPENAI_BASE_URL))
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&request);

            let mut es = EventSource::new(builder)
                .map_err(|e| anyhow::anyhow!("Failed to create SSE event source: {e:?}"))?;
            es.set_retry_policy(Box::new(NoRetry));

            let llm_stream = stream::unfold(
                OpenAIStreamState {
                    es,
                    tool_calls: HashMap::new(),
                    pending_completions: Vec::new(),
                    last_usage: None,
                    done: false,
                    api_key: api_key.clone(),
                    request: request.clone(),
                    retry_count: 0,
                },
                move |mut state| {
                    let client = client.clone();
                    async move {
                        if state.done {
                            return None;
                        }

                        // Emit pending tool completions one at a time.
                        if let Some(acc) = state.pending_completions.pop() {
                            if let Some(name) = acc.name {
                                let id = acc.id.clone();
                                return match serde_json::from_str(&acc.json_acc) {
                                    Ok(input) => Some((
                                        Ok(LlmEvent::ToolUseComplete { id, name, input }),
                                        state,
                                    )),
                                    Err(e) => Some((
                                        Ok(LlmEvent::Error {
                                            error: format!("Failed to parse tool input JSON: {e}"),
                                        }),
                                        state,
                                    )),
                                };
                            }
                        }

                        loop {
                            let item = match state.es.next().await {
                                Some(event) => event,
                                None => {
                                    // Stream ended without [DONE] — synthesize Done
                                    let usage = state.last_usage.as_ref().map(make_llm_usage);
                                    state.done = true;
                                    return Some((Ok(LlmEvent::Done { usage }), state));
                                }
                            };

                            let msg = match item {
                                Ok(Event::Open) => continue,
                                Ok(Event::Message(msg)) => msg,
                                Err(e) => {
                                    if state.retry_count < MAX_RETRIES && is_retryable(&e) {
                                        let backoff = backoff_duration(state.retry_count);
                                        tracing::warn!(
                                            retry_count = state.retry_count,
                                            backoff_ms = backoff.as_millis(),
                                            error = %e,
                                            "Retryable SSE transport error, backing off"
                                        );
                                        tokio::time::sleep(backoff).await;
                                        match EventSource::new(
                                            client
                                                .post(format!(
                                                    "{}/v1/chat/completions",
                                                    OPENAI_BASE_URL
                                                ))
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", state.api_key),
                                                )
                                                .json(&state.request),
                                        ) {
                                            Ok(mut new_es) => {
                                                new_es.set_retry_policy(Box::new(NoRetry));
                                                state.es = new_es;
                                                state.retry_count += 1;
                                                state.tool_calls.clear();
                                                state.pending_completions.clear();
                                                continue;
                                            }
                                            Err(rebuild_err) => {
                                                state.done = true;
                                                return Some((
                                                    Err(anyhow::anyhow!(
                                                        "EventSource rebuild failed: {rebuild_err:?}"
                                                    )),
                                                    state,
                                                ));
                                            }
                                        }
                                    }
                                    state.done = true;
                                    return Some((
                                        Err(anyhow::anyhow!(
                                            "SSE transport error after {} retries: {e}",
                                            state.retry_count
                                        )),
                                        state,
                                    ));
                                }
                            };

                            // Handle [DONE] sentinel (not valid JSON)
                            if msg.data == "[DONE]" {
                                let usage = state.last_usage.as_ref().map(make_llm_usage);
                                state.done = true;
                                return Some((Ok(LlmEvent::Done { usage }), state));
                            }

                            let chunk: OpenAISseChunk = match serde_json::from_str(&msg.data) {
                                Ok(chunk) => chunk,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        data = %msg.data,
                                        "Skipping unparseable SSE event"
                                    );
                                    continue;
                                }
                            };

                            // Stream the first choice (index 0); others are rare.
                            let choice = match chunk.choices.first() {
                                Some(c) => c,
                                None => continue,
                            };

                            // Emit reasoning content before regular content or tool calls.
                            // NOTE: If a chunk carries both `reasoning_content` and
                            // `content`, only the reasoning is emitted on this iteration;
                            // the content is dropped.  OpenAI's current API (as of 2026)
                            // sends them in separate chunks, but this assumption may
                            // change with future models.
                            if let Some(reasoning) = &choice.delta.reasoning_content {
                                if !reasoning.is_empty() {
                                    return Some((
                                        Ok(LlmEvent::Thinking {
                                            thinking: reasoning.clone(),
                                        }),
                                        state,
                                    ));
                                }
                            }

                            // Emit text content
                            if let Some(ref text) = choice.delta.content {
                                if !text.is_empty() {
                                    return Some((
                                        Ok(LlmEvent::Text { text: text.clone() }),
                                        state,
                                    ));
                                }
                            }

                            // Emit tool call deltas
                            if let Some(ref tool_deltas) = choice.delta.tool_calls {
                                for td in tool_deltas {
                                    let idx = td.index;
                                    let acc =
                                        state.tool_calls.entry(idx).or_insert_with(|| ToolAcc {
                                            id: String::new(),
                                            name: None,
                                            json_acc: String::new(),
                                        });

                                    if let Some(ref id) = td.id {
                                        acc.id = id.clone();
                                    }

                                    if let Some(ref func) = td.function {
                                        if let Some(ref name) = func.name {
                                            acc.name = Some(name.clone());
                                        }
                                        if let Some(ref args) = func.arguments {
                                            if !args.is_empty() {
                                                acc.json_acc.push_str(args);
                                                // NLL: borrow on state.tool_calls ends here
                                                let id = acc.id.clone();
                                                let name = acc.name.clone();
                                                return Some((
                                                    Ok(LlmEvent::ToolUseDelta {
                                                        id,
                                                        name,
                                                        input_json: args.clone(),
                                                    }),
                                                    state,
                                                ));
                                            }
                                        }
                                    }
                                }
                            }

                            // Handle finish_reason
                            match choice.finish_reason.as_deref() {
                                Some("stop") => {
                                    // Stash usage; Done emitted on [DONE]
                                    state.last_usage = chunk.usage.clone();
                                }
                                Some("tool_calls") => {
                                    // Drain accumulated tools into pending queue, sorted by
                                    // index so ToolUseComplete events are emitted in the
                                    // order the model declared them (HashMap iteration order
                                    // is non-deterministic).
                                    let mut ordered: Vec<(u32, ToolAcc)> =
                                        std::mem::take(&mut state.tool_calls).into_iter().collect();
                                    ordered.sort_unstable_by_key(|(idx, _)| *idx);
                                    state
                                        .pending_completions
                                        .extend(ordered.into_iter().map(|(_, acc)| acc));
                                }
                                Some("length") => {
                                    let text_content = if state.tool_calls.is_empty() {
                                        "response truncated by max_tokens"
                                    } else {
                                        "tool call truncated by max_tokens"
                                    };
                                    return Some((
                                        Ok(LlmEvent::Error {
                                            error: text_content.to_string(),
                                        }),
                                        state,
                                    ));
                                }
                                Some("content_filter") => {
                                    return Some((
                                        Ok(LlmEvent::Error {
                                            error: "content filter triggered".to_string(),
                                        }),
                                        state,
                                    ));
                                }
                                _ => {
                                    // null or unknown finish_reason — continue streaming
                                }
                            }
                            // If we reach here, the chunk didn't produce a visible event;
                            // continue the inner loop.
                        }
                    }
                },
            );

            Ok(Box::pin(llm_stream) as LlmStream)
        })
    }
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

    #[test]
    fn test_reasoning_content_deserialization() {
        // o-series models emit reasoning_content in the delta before content
        let json = r#"{
            "id": "chatcmpl_reason",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "reasoning_content": "Let me think step by step..."
                },
                "finish_reason": null
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        let delta = &chunk.choices[0].delta;
        assert_eq!(
            delta.reasoning_content.as_deref(),
            Some("Let me think step by step...")
        );
        assert!(delta.content.is_none());
    }

    #[test]
    fn test_reasoning_content_absent() {
        // Standard models (gpt-4, etc.) don't emit reasoning_content
        let json = r#"{
            "id": "chatcmpl_abc",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "Hello"},
                "finish_reason": null
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        let delta = &chunk.choices[0].delta;
        assert!(delta.reasoning_content.is_none());
        assert_eq!(delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_tool_call_chunk_with_finish_reason() {
        // Final tool call chunk signals tool_calls completion
        let json = r#"{
            "id": "chatcmpl_tools",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        assert_eq!(
            chunk.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_sse_chunk_with_content_filter() {
        let json = r#"{
            "id": "chatcmpl_filtered",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "content_filter"
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        assert_eq!(
            chunk.choices[0].finish_reason.as_deref(),
            Some("content_filter")
        );
    }

    #[test]
    fn test_sse_chunk_length_truncation() {
        let json = r#"{
            "id": "chatcmpl_trunc",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {"content": "truncated t"},
                "finish_reason": "length"
            }]
        }"#;
        let chunk: OpenAISseChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("length"));
        assert_eq!(
            chunk.choices[0].delta.content.as_deref(),
            Some("truncated t")
        );
    }

    #[test]
    fn test_messages_to_openai_system_and_user() {
        let messages = vec![Message::system("You are helpful."), Message::user("hello")];
        let result = messages_to_openai(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content.as_deref(), Some("You are helpful."));
        assert_eq!(result[1].role, "user");
        assert_eq!(result[1].content.as_deref(), Some("hello"));
    }

    #[test]
    fn test_messages_to_openai_empty() {
        let result = messages_to_openai(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_make_llm_usage_all_fields() {
        let usage = OpenAIUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
        };
        let llm_usage = make_llm_usage(&usage);
        assert_eq!(llm_usage.input_tokens, 100);
        assert_eq!(llm_usage.output_tokens, 50);
        assert!(llm_usage.cache_read_tokens.is_none());
        assert!(llm_usage.cache_creation_tokens.is_none());
    }

    #[test]
    fn test_make_llm_usage_partial() {
        let usage = OpenAIUsage {
            prompt_tokens: Some(30),
            completion_tokens: None,
            total_tokens: Some(30),
        };
        let llm_usage = make_llm_usage(&usage);
        assert_eq!(llm_usage.input_tokens, 30);
        assert_eq!(llm_usage.output_tokens, 0);
    }
}
