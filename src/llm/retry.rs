//! Shared retry policy and backoff helpers for SSE providers.
//!
//! Both Anthropic and OpenAI providers use the same retry strategy:
//! 3 attempts with exponential backoff (1s → 2s → 4s) for transient HTTP
//! errors (429 rate-limited, 529 overloaded). The retry policy prevents
//! EventSource auto-reconnection; retry is instead driven inside the
//! `stream::unfold` state machine by rebuilding the EventSource.

use std::time::Duration;

use reqwest_eventsource::retry::RetryPolicy;

/// Retry policy that never retries at the SSE transport layer.
///
/// Retry for transient HTTP errors (429, 529) is handled inside the
/// `stream::unfold` closure by rebuilding the `EventSource` from stored
/// request state. Letting `EventSource` auto-reconnect would create
/// duplicate requests and bypass our exponential backoff timing.
pub(crate) struct NoRetry;

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

/// Maximum number of retry attempts for transient HTTP errors (429, 529).
pub(crate) const MAX_RETRIES: u32 = 3;

/// Backoff base duration — first retry after 1s, then 2s, then 4s.
const BACKOFF_BASE_MS: u64 = 1000;

/// Check whether an SSE transport error is retryable.
///
/// Retryable errors are HTTP 429 (rate limited) and 529 (overloaded).
/// All other transport errors (connection refused, DNS failure, TLS) are
/// treated as terminal — they won't resolve within a short backoff window.
///
/// Uses `Debug` formatting to inspect the error representation, since
/// `reqwest_eventsource::Error` does not expose HTTP status codes
/// through its public API.
pub(crate) fn is_retryable(error: &impl std::fmt::Debug) -> bool {
    let msg = format!("{error:?}");
    msg.contains("429") || msg.contains("529")
}

/// Compute the backoff duration for the given retry attempt.
///
/// Exponential: base * 2^attempt.  Attempt 0 → 1s, attempt 1 → 2s,
/// attempt 2 → 4s.  Capped at 32s (unreachable with MAX_RETRIES = 3,
/// but serves as a safety net if MAX_RETRIES is increased).
pub(crate) fn backoff_duration(attempt: u32) -> Duration {
    let ms = BACKOFF_BASE_MS * 2u64.pow(attempt.min(5));
    Duration::from_millis(ms)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable_429() {
        // reqwest-eventsource formats HTTP 429 as "InvalidStatusCode(429)" in Debug.
        let msg = "InvalidStatusCode(429)";
        assert!(is_retryable(&msg), "429 should be retryable");
    }

    #[test]
    fn test_is_retryable_529() {
        let msg = "HTTP error: status code 529";
        assert!(is_retryable(&msg), "529 should be retryable");
    }

    #[test]
    fn test_is_retryable_non_http_error() {
        let msg = "connection refused";
        assert!(!is_retryable(&msg), "connection refused is not retryable");
    }

    #[test]
    fn test_is_retryable_500_not_retryable() {
        // 500 is an internal server error, not retryable (only 429/529)
        let msg = "InvalidStatusCode(500)";
        assert!(!is_retryable(&msg), "500 should NOT be retryable");
    }

    #[test]
    fn test_backoff_duration_exponential() {
        // Attempt 0 → 1s, attempt 1 → 2s, attempt 2 → 4s
        assert_eq!(backoff_duration(0), Duration::from_millis(1000));
        assert_eq!(backoff_duration(1), Duration::from_millis(2000));
        assert_eq!(backoff_duration(2), Duration::from_millis(4000));
    }

    #[test]
    fn test_backoff_duration_capped() {
        // Attempt 5 → 2^5 = 32s; attempt 6 should stay at 32s (2^5, capped)
        assert_eq!(backoff_duration(5), Duration::from_millis(32_000));
        assert_eq!(backoff_duration(6), Duration::from_millis(32_000));
        assert_eq!(backoff_duration(10), Duration::from_millis(32_000));
    }
}
