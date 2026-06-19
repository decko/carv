//! Core agent loop: prompt → LLM → tool → repeat.
//!
//! The agent loop wires together the LLM provider, tool registry, token budget,
//! and stream output formatter. It manages conversation state, dispatches tool
//! calls, and compacts history when approaching the context window limit.
//!
//! ## Loop flow
//! 1. Build messages: system prompt + user prompt
//! 2. Call provider (retry handled internally by provider layer)
//! 3. For each [`LlmEvent`]:
//!    - Text → emit via output
//!    - Thinking → emit via output
//!    - ToolUseDelta → accumulate JSON fragments per call ID
//!    - ToolUseComplete → add to assistant content, remove accumulator
//!    - Done → record usage, exit loop
//!    - Error → terminal, return error
//! 4. Emit [`StreamEvent::ToolUse`] for each tool, then dispatch → emit
//!    [`StreamEvent::ToolResult`]
//! 5. If tool executed, check token budget → compact if needed → goto 2
//! 6. Stop on Done or max_turns reached
//! 7. Emit final summary

use std::collections::HashMap;

use anyhow::Result;
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::agent::budget::TokenBudget;
use crate::llm::provider::LlmProvider;
use crate::llm::types::{ContentType, LlmEvent, LlmUsage, Message, RequestConfig, Role, ToolDef};
use crate::stream::output::{StreamEvent, StreamOutput, Usage};
use crate::tools::registry::ToolRegistry;
use crate::tools::traits::{ToolContext, ToolResult as ToolExecResult};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Summary emitted at the end of an agent run.
#[derive(Debug, Clone)]
pub struct AgentSummary {
    /// Number of conversation turns completed (tool-invocation turns only).
    pub turns: u32,
    /// Total input tokens across all LLM calls.
    pub total_input_tokens: u64,
    /// Total output tokens across all LLM calls.
    pub total_output_tokens: u64,
    /// Estimated cost in USD (heuristic: $3/M input, $15/M output — approximate,
    /// model-specific pricing may differ).
    pub estimated_cost_usd: f32,
}

// ---------------------------------------------------------------------------
// Per-turn accumulator
// ---------------------------------------------------------------------------

/// Accumulates partial JSON fragments for an in-flight tool call.
struct Acc {
    tool_name: String,
    json_parts: Vec<String>,
}

impl Acc {
    fn new(name: String) -> Self {
        Acc {
            tool_name: name,
            json_parts: Vec::new(),
        }
    }

    fn push(&mut self, fragment: String) {
        self.json_parts.push(fragment);
    }

    fn input_json(&self) -> String {
        self.json_parts.join("")
    }
}

// ---------------------------------------------------------------------------
// Core loop
// ---------------------------------------------------------------------------

/// Run the agent loop: prompt → LLM → tool → repeat.
///
/// See the [module-level documentation](self) for the full loop flow.
///
/// # Panics
///
/// This function does not panic. All errors are returned as `Result::Err`.
///
/// # Arguments
///
/// The argument count is high because this is the central orchestration
/// point that wires together the provider, registry, context, budget,
/// prompts, config, and output — all of which are independently configured.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip(provider, registry, tool_ctx, output))]
pub async fn run_agent_loop(
    provider: &dyn LlmProvider,
    registry: &ToolRegistry,
    tool_ctx: &ToolContext,
    system_prompt: &str,
    user_prompt: &str,
    request_config: &RequestConfig,
    max_turns: u32,
    context_window: usize,
    output: &mut dyn StreamOutput,
) -> Result<AgentSummary> {
    let mut budget = TokenBudget::new(context_window);
    let tool_defs = registry.tool_defs();

    // Build initial messages: system + user
    let mut messages: Vec<Message> =
        vec![Message::system(system_prompt), Message::user(user_prompt)];

    let mut turns: u32 = 0;

    loop {
        // -- Token budget check before each LLM call --
        let estimated = budget.estimate_payload(system_prompt, &messages, &tool_defs);
        if budget.is_over_threshold(estimated) {
            debug!(
                estimated,
                cumulative_input = budget.total_input(),
                cumulative_output = budget.total_output(),
                msg_count = messages.len(),
                "context window threshold reached, compacting"
            );
            compact_conversation(&mut messages);
        }

        // -- Call LLM (retry handled internally by provider) --
        let assistant_msg =
            call_llm(provider, &messages, &tool_defs, request_config, output).await?;

        // -- Record usage --
        if let Some(ref u) = assistant_msg.usage {
            budget.record_usage(u);
        }

        // -- Check for tool calls in the assistant response --
        let tool_calls: Vec<(ContentType, String)> = assistant_msg
            .message
            .content
            .iter()
            .filter_map(|block| match &block.content {
                ContentType::ToolUse { name, input, .. } => {
                    Some((block.content.clone(), format!("{name}({input})")))
                }
                _ => None,
            })
            .collect();

        messages.push(assistant_msg.message);

        if tool_calls.is_empty() {
            // Text-only response — agent is done
            info!(turns, "agent finished — no tool invocation");
            break;
        }

        // -- Emit tool use events and dispatch each tool call --
        for (content, call_desc) in &tool_calls {
            if let ContentType::ToolUse {
                ref id,
                ref name,
                ref input,
            } = content
            {
                // Emit tool use event before dispatch
                output
                    .emit(StreamEvent::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    })
                    .await?;

                let result = match registry.get(name) {
                    Some(tool) => {
                        debug!(%name, tool_call_id = %id, "dispatching tool");
                        match tool.execute(input.clone(), tool_ctx).await {
                            Ok(r) => r,
                            Err(e) => ToolExecResult::error(format!(
                                "tool {name} execution failed: {e:#}"
                            )),
                        }
                    }
                    None => {
                        warn!(%name, "unknown tool requested by LLM");
                        ToolExecResult::error(format!("unknown tool: {name}"))
                    }
                };

                emit_tool_result(output, id, &result).await?;

                messages.push(Message::tool_result(id.clone(), result.content));

                debug!(
                    %call_desc,
                    is_error = result.is_error,
                    "tool call dispatched"
                );
            }
        }

        turns += 1;
        debug!(turns, max_turns, "tool turn completed");

        if turns >= max_turns {
            info!(turns, "max turns reached");
            break;
        }
    }

    // -- Emit final summary from budget counters --
    let total_input = budget.total_input();
    let total_output = budget.total_output();
    let estimated_cost = (total_input as f32 * 3.0 + total_output as f32 * 15.0) / 1_000_000.0;

    output
        .emit(StreamEvent::Done {
            turns,
            usage: Usage {
                input_tokens: u64_to_u32_trunc(total_input),
                output_tokens: u64_to_u32_trunc(total_output),
                cache_read_tokens: 0,
            },
        })
        .await?;
    output.finish().await?;

    Ok(AgentSummary {
        turns,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        estimated_cost_usd: estimated_cost,
    })
}

// ---------------------------------------------------------------------------
// LLM call
// ---------------------------------------------------------------------------

/// Result of a single LLM streaming call.
struct LlmCallResult {
    /// The assistant's response message (text + tool_use blocks).
    message: Message,
    /// Token usage (may be None if the provider didn't report it).
    usage: Option<LlmUsage>,
}

/// Call the LLM provider and drain the stream into a [`LlmCallResult`].
///
/// Retry for transient transport errors (429/529) is handled **internally**
/// by the provider layer — see [`crate::llm::retry`]. This function does not
/// add a second retry layer to avoid retry multiplication.
async fn call_llm(
    provider: &dyn LlmProvider,
    messages: &[Message],
    tools: &[ToolDef],
    config: &RequestConfig,
    output: &mut dyn StreamOutput,
) -> Result<LlmCallResult> {
    let mut stream = provider.stream_chat(messages, tools, config).await?;

    let mut assistant_content: Vec<crate::llm::types::ContentBlock> = Vec::new();
    let mut usage: Option<LlmUsage> = None;
    let mut accumulators: HashMap<String, Acc> = HashMap::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => match event {
                LlmEvent::Text { text } => {
                    assistant_content.push(crate::llm::types::ContentBlock::text(&text));
                    output.emit(StreamEvent::Text { content: text }).await?;
                }
                LlmEvent::Thinking { thinking } => {
                    output
                        .emit(StreamEvent::Thinking { content: thinking })
                        .await?;
                }
                LlmEvent::ToolUseDelta {
                    id,
                    name,
                    input_json,
                } => {
                    let acc = accumulators
                        .entry(id.clone())
                        .or_insert_with(|| Acc::new(name.unwrap_or_default()));
                    acc.push(input_json);
                }
                LlmEvent::ToolUseComplete { id, name, input } => {
                    // Tool call is complete — add to assistant content
                    // and remove the accumulator to prevent double-emit.
                    accumulators.remove(&id);
                    assistant_content.push(crate::llm::types::ContentBlock {
                        content: ContentType::ToolUse { id, name, input },
                        cache_control: None,
                    });
                }
                LlmEvent::Done { usage: u } => {
                    let input = u.as_ref().map(|x| x.input_tokens);
                    let output = u.as_ref().map(|x| x.output_tokens);
                    usage = u;
                    debug!(?input, ?output, "LLM call complete");
                    break;
                }
                LlmEvent::Error { error } => {
                    return Err(anyhow::anyhow!("LLM protocol error: {error}"));
                }
            },
            Err(e) => {
                return Err(anyhow::anyhow!("LLM transport error: {e:#}"));
            }
        }
    }

    // Drain any remaining accumulators (partial tool calls that didn't
    // get a ToolUseComplete event — edge case).
    for (id, acc) in accumulators.drain() {
        let json = acc.input_json();
        if let Ok(input) = serde_json::from_str::<serde_json::Value>(&json) {
            debug!(%id, tool_name = %acc.tool_name, "finalizing tool call from fragments");
            assistant_content.push(crate::llm::types::ContentBlock {
                content: ContentType::ToolUse {
                    id,
                    name: acc.tool_name,
                    input,
                },
                cache_control: None,
            });
        } else {
            warn!(%id, fragment_len = json.len(), "failed to parse accumulated tool call JSON");
        }
    }

    let message = Message {
        role: Role::Assistant,
        content: assistant_content,
    };

    Ok(LlmCallResult { message, usage })
}

// ---------------------------------------------------------------------------
// History compaction
// ---------------------------------------------------------------------------

/// Compact conversation history to stay within the context window.
///
/// Strategy: keep the system message + initial user prompt, plus the most
/// recent 3 **turns**. A turn is one assistant message (with at least one
/// `ToolUse` block) followed by all subsequent `Tool`-role result messages
/// until the next assistant message. This correctly handles multi-tool turns.
fn compact_conversation(messages: &mut Vec<Message>) {
    // Need at least 4 messages (system, user, assistant, tool_result) to compact.
    if messages.len() <= 3 {
        return;
    }

    // Find turn boundaries. A turn starts at an assistant message that
    // contains at least one ToolUse block.
    let mut turn_starts: Vec<usize> = Vec::new();
    let mut i = 2; // skip system (0) and initial user (1)
    while i < messages.len() {
        let is_tool_use_assistant = messages[i].role == Role::Assistant
            && messages[i]
                .content
                .iter()
                .any(|b| matches!(b.content, ContentType::ToolUse { .. }));
        if is_tool_use_assistant {
            turn_starts.push(i);
        }
        i += 1;
    }

    // Keep at most 3 most recent turns
    if turn_starts.len() <= 3 {
        return;
    }

    let drop_count = turn_starts.len() - 3;
    let first_keep_idx = turn_starts[drop_count];

    debug!(
        total_turns = turn_starts.len(),
        drop_count, first_keep_idx, "compacting conversation: dropping oldest turns"
    );

    // Keep messages[0..2] (system + user) + messages[first_keep_idx..]
    let tail = messages.split_off(first_keep_idx);
    messages.truncate(2);
    messages.extend(tail);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Safely convert a `u64` token count to `u32`, capping at `u32::MAX`.
fn u64_to_u32_trunc(n: u64) -> u32 {
    n.min(u32::MAX as u64) as u32
}

/// Emit a tool result event through the output formatter.
async fn emit_tool_result(
    output: &mut dyn StreamOutput,
    id: &str,
    result: &ToolExecResult,
) -> Result<()> {
    output
        .emit(StreamEvent::ToolResult {
            id: id.to_string(),
            content: if result.is_error {
                format!("Error: {}", result.content)
            } else {
                result.content.clone()
            },
        })
        .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::ContentBlock;

    // -- Helper factories --

    fn tool_use_msg(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock {
                content: ContentType::ToolUse {
                    id: id.into(),
                    name: "mock".into(),
                    input: serde_json::json!({}),
                },
                cache_control: None,
            }],
        }
    }

    fn tool_result_msg(id: &str) -> Message {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock {
                content: ContentType::ToolResult {
                    tool_use_id: id.into(),
                    content: "ok".into(),
                    is_error: false,
                },
                cache_control: None,
            }],
        }
    }

    // -- Initial guard clauses --

    #[test]
    fn test_compact_no_tool_pairs_preserves_messages() {
        let mut msgs = vec![
            Message::system("system"),
            Message::user("hello"),
            Message::assistant("response without tools"),
        ];
        let original_len = msgs.len();
        compact_conversation(&mut msgs);
        assert_eq!(msgs.len(), original_len);
    }

    #[test]
    fn test_compact_too_few_messages() {
        let mut msgs = vec![Message::system("s"), Message::user("u")];
        let original_len = msgs.len();
        compact_conversation(&mut msgs);
        assert_eq!(msgs.len(), original_len);
    }

    #[test]
    fn test_compact_empty() {
        let mut msgs: Vec<Message> = vec![];
        compact_conversation(&mut msgs);
        assert!(msgs.is_empty());
    }

    // -- Single-tool-per-turn compaction --

    #[test]
    fn test_compact_drops_old_turns_keeps_recent() {
        // 5 turns, each with 1 tool call → 10 tool msgs + 2 base = 12 total
        let mut msgs = vec![Message::system("s"), Message::user("u")];
        for i in 0..5 {
            msgs.push(tool_use_msg(&format!("call_{i}")));
            msgs.push(tool_result_msg(&format!("call_{i}")));
        }
        assert_eq!(msgs.len(), 12);

        compact_conversation(&mut msgs);

        // Expected: system + user + last 3 turns (6 msgs) = 8 total
        assert_eq!(
            msgs.len(),
            8,
            "should keep system + user + 3 most recent turns"
        );
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[1].role, Role::User);

        // Verify kept turns are pairs 2, 3, 4
        for turn_idx in 0..3 {
            let msg_idx = 2 + turn_idx * 2;
            let call_num = 2 + turn_idx;
            match &msgs[msg_idx].content[0].content {
                ContentType::ToolUse { id, .. } => {
                    assert_eq!(id, &format!("call_{call_num}"));
                }
                _ => panic!("expected ToolUse"),
            }
            match &msgs[msg_idx + 1].content[0].content {
                ContentType::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, &format!("call_{call_num}"));
                }
                _ => panic!("expected ToolResult"),
            }
        }
    }

    #[test]
    fn test_compact_fewer_than_keep_threshold() {
        // 2 turns = below keep threshold of 3
        let mut msgs = vec![Message::system("s"), Message::user("u")];
        msgs.push(tool_use_msg("call_0"));
        msgs.push(tool_result_msg("call_0"));
        msgs.push(tool_use_msg("call_1"));
        msgs.push(tool_result_msg("call_1"));
        let original_len = msgs.len();
        compact_conversation(&mut msgs);
        assert_eq!(msgs.len(), original_len);
    }

    // -- Multi-tool-per-turn compaction --

    #[test]
    fn test_compact_multi_tool_turn_preserves_all_results() {
        // Turn 0: assistant with 2 tool_use blocks → 2 results → 3 msgs
        // Turn 1: assistant with 1 tool_use block  → 1 result  → 2 msgs
        // etc.
        let mut msgs = vec![Message::system("s"), Message::user("u")];

        // Turn 0 (multi-tool: 2 calls)
        msgs.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock {
                    content: ContentType::ToolUse {
                        id: "call_0a".into(),
                        name: "mock".into(),
                        input: serde_json::json!({}),
                    },
                    cache_control: None,
                },
                ContentBlock {
                    content: ContentType::ToolUse {
                        id: "call_0b".into(),
                        name: "mock".into(),
                        input: serde_json::json!({}),
                    },
                    cache_control: None,
                },
            ],
        });
        msgs.push(tool_result_msg("call_0a"));
        msgs.push(tool_result_msg("call_0b"));

        // Turn 1
        msgs.push(tool_use_msg("call_1"));
        msgs.push(tool_result_msg("call_1"));

        // Turn 2
        msgs.push(tool_use_msg("call_2"));
        msgs.push(tool_result_msg("call_2"));

        // Turn 3
        msgs.push(tool_use_msg("call_3"));
        msgs.push(tool_result_msg("call_3"));

        // Turn 4 (multi-tool: 3 calls)
        msgs.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock {
                    content: ContentType::ToolUse {
                        id: "call_4a".into(),
                        name: "mock".into(),
                        input: serde_json::json!({}),
                    },
                    cache_control: None,
                },
                ContentBlock {
                    content: ContentType::ToolUse {
                        id: "call_4b".into(),
                        name: "mock".into(),
                        input: serde_json::json!({}),
                    },
                    cache_control: None,
                },
                ContentBlock {
                    content: ContentType::ToolUse {
                        id: "call_4c".into(),
                        name: "mock".into(),
                        input: serde_json::json!({}),
                    },
                    cache_control: None,
                },
            ],
        });
        msgs.push(tool_result_msg("call_4a"));
        msgs.push(tool_result_msg("call_4b"));
        msgs.push(tool_result_msg("call_4c"));

        // 2 base + 3 + 2 + 2 + 2 + 4 = 15 messages
        assert_eq!(msgs.len(), 15);

        compact_conversation(&mut msgs);

        // Kept: system + user + turns 2,3,4
        // Turn 2: 2 msgs, Turn 3: 2 msgs, Turn 4: 4 msgs = 8 + 2 = 10
        assert_eq!(
            msgs.len(),
            10,
            "should keep system + user + 3 most recent turns (including multi-tool)"
        );
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[1].role, Role::User);

        // Turn 2 assistant at index 2
        match &msgs[2].content[0].content {
            ContentType::ToolUse { id, .. } => assert_eq!(id, "call_2"),
            _ => panic!("expected ToolUse call_2"),
        }
        // Turn 4 should have all 3 results
        let last_assistant_idx = msgs.len() - 4; // 4 msgs in turn 4
        match &msgs[last_assistant_idx].content[2].content {
            ContentType::ToolUse { id, .. } => assert_eq!(id, "call_4c"),
            _ => panic!("expected ToolUse call_4c"),
        }
    }

    // -- AgentSummary --

    #[test]
    fn test_agent_summary_fields() {
        let summary = AgentSummary {
            turns: 5,
            total_input_tokens: 10_000,
            total_output_tokens: 2_000,
            estimated_cost_usd: 0.06,
        };
        assert_eq!(summary.turns, 5);
        assert_eq!(summary.total_input_tokens, 10_000);
        assert_eq!(summary.total_output_tokens, 2_000);
        assert!((summary.estimated_cost_usd - 0.06).abs() < f32::EPSILON);
    }

    // -- Acc --

    #[test]
    fn test_acc_accumulation() {
        let mut acc = Acc::new("read_file".into());
        acc.push(r#"{"path": "/t"#.into());
        acc.push(r#"mp/foo"}"#.into());
        assert_eq!(acc.input_json(), r#"{"path": "/tmp/foo"}"#);
        assert_eq!(acc.tool_name, "read_file");
    }

    // -- u64_to_u32_trunc --

    #[test]
    fn test_u64_to_u32_trunc_normal() {
        assert_eq!(u64_to_u32_trunc(42), 42);
        assert_eq!(u64_to_u32_trunc(0), 0);
    }

    #[test]
    fn test_u64_to_u32_trunc_large() {
        assert_eq!(u64_to_u32_trunc(1_000_000), 1_000_000);
    }

    #[test]
    fn test_u64_to_u32_trunc_caps_at_u32_max() {
        assert_eq!(u64_to_u32_trunc(u64::MAX), u32::MAX);
        assert_eq!(u64_to_u32_trunc(u32::MAX as u64 + 1), u32::MAX);
    }
}
