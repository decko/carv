//! Token budget tracking and context window truncation.
//!
//! Tracks cumulative token usage across agent turns and truncates
//! conversation history when approaching the model's context window
//! (80% threshold by default).

use crate::llm::types::LlmUsage;

/// Token budget tracker for context window management.
///
/// Before each LLM call, the agent estimates the message payload size
/// and truncates old tool results if the threshold is exceeded. If
/// truncating tool results isn't enough, the oldest conversation
/// turns are dropped entirely.
pub struct TokenBudget {
    context_window: usize,
    threshold: f64,
    total_input: u64,
    total_output: u64,
}

impl TokenBudget {
    /// Create a new budget with the given context window size.
    ///
    /// Defaults: 80% threshold.
    pub fn new(context_window: usize) -> Self {
        Self {
            context_window,
            threshold: 0.8,
            total_input: 0,
            total_output: 0,
        }
    }

    /// Record token usage from a completed turn.
    pub fn record_usage(&mut self, usage: &LlmUsage) {
        self.total_input += usage.input_tokens as u64;
        self.total_output += usage.output_tokens as u64;
        if let Some(cache) = usage.cache_read_tokens {
            self.total_input += cache as u64;
        }
    }

    /// Estimated token count for a messages array.
    ///
    /// Uses a simple heuristic: ~4 characters per token for English text.
    /// Not exact, but sufficient for the 80% threshold check.
    pub fn estimate(&self, messages_json: &str) -> usize {
        messages_json.len() / 4
    }

    /// Whether the estimated payload exceeds the threshold.
    pub fn is_over_threshold(&self, estimated: usize) -> bool {
        let limit = (self.context_window as f64 * self.threshold) as usize;
        estimated >= limit
    }

    /// Context window size.
    pub fn context_window(&self) -> usize {
        self.context_window
    }

    /// Current threshold fraction.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Total input tokens across all turns.
    pub fn total_input(&self) -> u64 {
        self.total_input
    }

    /// Total output tokens across all turns.
    pub fn total_output(&self) -> u64 {
        self.total_output
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

    #[test]
    fn test_new_budget_defaults() {
        let budget = TokenBudget::new(200_000);
        assert_eq!(budget.context_window(), 200_000);
        assert!((budget.threshold() - 0.8).abs() < f64::EPSILON);
        assert_eq!(budget.total_input(), 0);
        assert_eq!(budget.total_output(), 0);
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
    fn test_record_usage_with_cache_read() {
        let mut budget = TokenBudget::new(100_000);
        let usage = LlmUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: Some(300),
            cache_creation_tokens: Some(100),
        };
        budget.record_usage(&usage);
        // cache_read counts toward input
        assert_eq!(budget.total_input(), 1300);
        assert_eq!(budget.total_output(), 500);
    }

    #[test]
    fn test_estimate_approximates_tokens() {
        let budget = TokenBudget::new(100_000);
        let json = r#"{"messages":[{"role":"user","content":"hello world"}]}"#;
        let tokens = budget.estimate(json);
        // ~74 chars / 4 ≈ 18 tokens
        assert!(tokens > 10, "estimated {tokens} should be > 10");
        assert!(tokens < 30, "estimated {tokens} should be < 30");
    }

    #[test]
    fn test_is_over_threshold_below() {
        let budget = TokenBudget::new(100_000);
        // 100k * 0.8 = 80k.  10k is well below.
        assert!(!budget.is_over_threshold(10_000));
    }

    #[test]
    fn test_is_over_threshold_at_boundary() {
        let budget = TokenBudget::new(100_000);
        // 100k * 0.8 = 80k.  79_999 is below, 80_000 is at threshold.
        assert!(!budget.is_over_threshold(79_999));
        assert!(budget.is_over_threshold(80_000));
    }

    #[test]
    fn test_is_over_threshold_above() {
        let budget = TokenBudget::new(100_000);
        assert!(budget.is_over_threshold(90_000));
    }

    #[test]
    fn test_total_tokens_includes_cache() {
        let mut budget = TokenBudget::new(200_000);
        // 3 turns with cache hits
        for _ in 0..3 {
            let usage = LlmUsage {
                input_tokens: 2000,
                output_tokens: 1000,
                cache_read_tokens: Some(1500),
                cache_creation_tokens: None,
            };
            budget.record_usage(&usage);
        }
        // input = 3 * (2000 + 1500) = 10500
        assert_eq!(budget.total_input(), 10500);
        assert_eq!(budget.total_output(), 3000);
    }

    #[test]
    fn test_different_context_windows() {
        let small = TokenBudget::new(32_000);
        let large = TokenBudget::new(200_000);
        // 32k * 0.8 = 25600
        assert!(!small.is_over_threshold(20_000));
        assert!(small.is_over_threshold(30_000));
        // 200k * 0.8 = 160000
        assert!(!large.is_over_threshold(20_000));
        assert!(!large.is_over_threshold(100_000));
    }
}
