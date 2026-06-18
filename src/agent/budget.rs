//! Token budget tracking and context window management.
//!
//! Tracks cumulative token usage across agent turns and provides
//! threshold checks for the core loop. Truncation logic lives in
//! the loop module (H3) — the budget only reports whether the
//! threshold is exceeded.

use crate::llm::types::{LlmUsage, Message, ToolDef};

/// Token budget tracker for context window management.
///
/// Before each LLM call, the agent estimates the message payload size
/// and checks whether the 80% context window threshold has been
/// exceeded. Both the current payload estimate and cumulative usage
/// via `record_usage` are considered.
pub struct TokenBudget {
    context_window: usize,
    threshold: f64,
    total_input: u64,
    total_output: u64,
    total_cache_creation: u64,
}

impl TokenBudget {
    /// Create a new budget with the given context window size.
    ///
    /// Defaults: 80% threshold.
    ///
    /// # Panics
    ///
    /// Panics if `context_window` is too small to produce a
    /// meaningful threshold (i.e. the 80% limit would round to zero).
    pub fn new(context_window: usize) -> Self {
        assert!(
            (context_window as f64 * 0.8) as usize > 0,
            "context_window {context_window} too small: 80% threshold rounds to zero"
        );
        Self {
            context_window,
            threshold: 0.8,
            total_input: 0,
            total_output: 0,
            total_cache_creation: 0,
        }
    }

    /// Record token usage from a completed turn.
    ///
    /// `input_tokens` already includes cache read tokens in the
    /// Anthropic wire format — do not double-count `cache_read_tokens`.
    pub fn record_usage(&mut self, usage: &LlmUsage) {
        self.total_input += usage.input_tokens as u64;
        self.total_output += usage.output_tokens as u64;
        if let Some(cache) = usage.cache_creation_tokens {
            self.total_cache_creation += cache as u64;
        }
    }

    /// Estimate the token count of the full LLM payload.
    ///
    /// Covers system prompt, conversation history, and tool definitions.
    /// Uses a simple heuristic: ~4 characters per token for English text.
    /// Serializes components to JSON internally.
    ///
    /// On serialization failure, falls back to a conservative (large)
    /// estimate so the budget errs toward overestimation rather than
    /// silently undercounting.
    pub fn estimate_payload(
        &self,
        system_prompt: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> usize {
        let sys_len = system_prompt.len();
        let msg_len = serde_json::to_string(messages)
            .map(|s| s.len())
            .unwrap_or(usize::MAX / 2);
        let tool_len = serde_json::to_string(tools)
            .map(|s| s.len())
            .unwrap_or(usize::MAX / 2);
        (sys_len.saturating_add(msg_len).saturating_add(tool_len)) / 4
    }

    /// Whether the estimated payload or cumulative usage exceeds the
    /// threshold. Considers both the current payload estimate and the
    /// running total from `record_usage` calls.
    ///
    /// Uses `u64` arithmetic to avoid overflow on 32-bit targets where
    /// `usize` is 32 bits but cumulative usage can exceed 4 billion.
    pub fn is_over_threshold(&self, estimated: usize) -> bool {
        let limit = (self.context_window as f64 * self.threshold) as u64;
        let cumulative = self.total_input.saturating_add(self.total_output);
        (estimated as u64) >= limit || cumulative >= limit
    }

    /// Context window size.
    pub fn context_window(&self) -> usize {
        self.context_window
    }

    /// Current threshold fraction.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Total input tokens across all turns (includes cache reads).
    pub fn total_input(&self) -> u64 {
        self.total_input
    }

    /// Total output tokens across all turns.
    pub fn total_output(&self) -> u64 {
        self.total_output
    }

    /// Total cache creation tokens across all turns (billing-only,
    /// not context-window impacting).
    pub fn total_cache_creation(&self) -> u64 {
        self.total_cache_creation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_usage(input: u32, output: u32) -> LlmUsage {
        LlmUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }
    }

    fn make_msg(role: &str, text: &str) -> Message {
        use crate::llm::types::{ContentBlock, Role};
        Message {
            role: if role == "system" {
                Role::System
            } else {
                Role::User
            },
            content: vec![ContentBlock::text(text)],
        }
    }

    fn make_tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({}),
        }
    }

    #[test]
    #[should_panic(expected = "too small")]
    fn test_new_panics_on_zero_window() {
        TokenBudget::new(0);
    }

    #[test]
    #[should_panic(expected = "too small")]
    fn test_new_panics_on_still_zero_threshold() {
        // context_window=1 → (1 * 0.8) as usize = 0, same problem
        TokenBudget::new(1);
    }

    #[test]
    fn test_new_minimum_viable_window() {
        // context_window=2 → (2 * 0.8) as usize = 1, works
        let budget = TokenBudget::new(2);
        assert_eq!(budget.context_window(), 2);
    }

    #[test]
    fn test_new_budget_defaults() {
        let budget = TokenBudget::new(200_000);
        assert_eq!(budget.context_window(), 200_000);
        assert!((budget.threshold() - 0.8).abs() < f64::EPSILON);
        assert_eq!(budget.total_input(), 0);
        assert_eq!(budget.total_output(), 0);
        assert_eq!(budget.total_cache_creation(), 0);
    }

    #[test]
    fn test_record_usage_accumulates() {
        let mut budget = TokenBudget::new(100_000);
        budget.record_usage(&make_usage(1000, 500));
        budget.record_usage(&make_usage(2000, 800));
        assert_eq!(budget.total_input(), 3000);
        assert_eq!(budget.total_output(), 1300);
    }

    #[test]
    fn test_record_usage_cache_read_not_double_counted() {
        // Anthropic's input_tokens already includes cache_read_tokens.
        // The budget should NOT add cache_read_tokens separately.
        let mut budget = TokenBudget::new(100_000);
        let usage = LlmUsage {
            input_tokens: 1000, // total input including cache reads
            output_tokens: 500,
            cache_read_tokens: Some(300), // included in input_tokens above
            cache_creation_tokens: Some(100),
        };
        budget.record_usage(&usage);
        assert_eq!(budget.total_input(), 1000);
        assert_eq!(budget.total_output(), 500);
        assert_eq!(budget.total_cache_creation(), 100);
    }

    #[test]
    fn test_estimate_payload_with_messages_only() {
        let budget = TokenBudget::new(100_000);
        let msgs = vec![make_msg("user", "hello world")];
        let tokens = budget.estimate_payload("", &msgs, &[]);
        assert!(tokens > 5, "estimated {tokens} should be > 5");
        assert!(tokens < 50, "estimated {tokens} should be < 50");
    }

    #[test]
    fn test_estimate_payload_includes_system_prompt() {
        let budget = TokenBudget::new(100_000);
        let msgs = vec![make_msg("user", "hello")];
        let without_sys = budget.estimate_payload("", &msgs, &[]);
        let with_sys = budget.estimate_payload("You are a helpful agent.", &msgs, &[]);
        assert!(
            with_sys > without_sys,
            "system prompt should increase estimate: {with_sys} vs {without_sys}"
        );
    }

    #[test]
    fn test_estimate_payload_includes_tools() {
        let budget = TokenBudget::new(100_000);
        let msgs = vec![make_msg("user", "hello")];
        let tools = vec![make_tool("read_file"), make_tool("write_file")];
        let without_tools = budget.estimate_payload("", &msgs, &[]);
        let with_tools = budget.estimate_payload("", &msgs, &tools);
        assert!(
            with_tools > without_tools,
            "tools should increase estimate: {with_tools} vs {without_tools}"
        );
    }

    #[test]
    fn test_is_over_threshold_below() {
        let budget = TokenBudget::new(100_000);
        assert!(!budget.is_over_threshold(10_000));
    }

    #[test]
    fn test_is_over_threshold_at_boundary() {
        let budget = TokenBudget::new(100_000);
        assert!(!budget.is_over_threshold(79_999));
        assert!(budget.is_over_threshold(80_000));
    }

    #[test]
    fn test_is_over_threshold_above() {
        let budget = TokenBudget::new(100_000);
        assert!(budget.is_over_threshold(90_000));
    }

    #[test]
    fn test_is_over_threshold_from_cumulative_usage() {
        let mut budget = TokenBudget::new(100_000);
        // 80k limit. After recording 81k of usage, it should fire
        // even with a zero estimate.
        budget.record_usage(&make_usage(80_000, 1_000));
        assert!(budget.is_over_threshold(0));
    }

    #[test]
    fn test_is_over_threshold_cumulative_not_yet_reached() {
        let mut budget = TokenBudget::new(100_000);
        budget.record_usage(&make_usage(30_000, 10_000));
        // 40k cumulative < 80k limit, 10k estimate < 80k limit
        assert!(!budget.is_over_threshold(10_000));
    }

    #[test]
    fn test_different_context_windows() {
        let small = TokenBudget::new(32_000);
        let large = TokenBudget::new(200_000);
        assert!(!small.is_over_threshold(20_000));
        assert!(small.is_over_threshold(30_000));
        assert!(!large.is_over_threshold(20_000));
        assert!(!large.is_over_threshold(100_000));
    }
}
