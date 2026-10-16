//! Token-bucket rate limiter for the MCP server.
//!
//! A configurable rate limiter that enforces a maximum requests-per-second
//! rate. When the limit is exceeded, the limiter returns the number of
//! seconds to wait before retrying.
//!
//! REV-WARPFS-5: the limiter also carries the server's backpressure signal
//! and metrics snapshot. "Queue depth" is the number of tokens already
//! consumed from the bucket (the burst the server is servicing beyond its
//! sustained rate); when that depth crosses 80% of capacity, responses carry
//! a `"warning": "high queue depth"` field so well-behaved clients can slow
//! down before they start getting `-32000` rejections.

use std::time::Instant;

#[cfg(test)]
use std::time::Duration;

/// Queue-depth fraction (of bucket capacity) above which the limiter
/// reports backpressure. Depth is `(capacity - tokens)`; at or above 80%
/// of capacity consumed, [`RateLimiter::under_backpressure`] is true.
pub const BACKPRESSURE_THRESHOLD: f64 = 0.8;

/// The warning string attached to JSON-RPC responses under backpressure.
pub const BACKPRESSURE_WARNING: &str = "high queue depth";

/// A token-bucket rate limiter.
///
/// Tokens are replenished at a fixed rate (requests per second).
/// Each `check()` call consumes one token if available.
/// When the bucket is empty, `check()` returns `None`.
#[derive(Debug)]
pub struct RateLimiter {
    /// Maximum tokens in the bucket (burst capacity).
    capacity: u32,
    /// Current tokens available.
    tokens: f64,
    /// Tokens added per second.
    rate: f64,
    /// Last time tokens were replenished.
    last_refill: Instant,
    /// Requests admitted since creation (REV-WARPFS-5 metrics).
    admitted: u64,
    /// Requests rejected by the token bucket since creation.
    rejected: u64,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// `rate_rps` is the sustained rate in requests per second.
    /// If `rate_rps` is 0, rate limiting is disabled.
    pub fn new(rate_rps: u32) -> Self {
        let rate = rate_rps as f64;
        Self {
            capacity: rate_rps.max(1),
            tokens: rate,
            rate,
            last_refill: Instant::now(),
            admitted: 0,
            rejected: 0,
        }
    }

    /// Check if a request is allowed. Returns `true` if allowed,
    /// `false` if rate-limited (caller should wait).
    ///
    /// Each successful check consumes one token. Tokens are
    /// replenished automatically based on elapsed time.
    pub fn check(&mut self) -> bool {
        if self.rate == 0.0 {
            self.admitted += 1;
            return true; // Rate limiting disabled
        }
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            self.admitted += 1;
            true
        } else {
            self.rejected += 1;
            false
        }
    }

    /// Seconds until the next token will be available.
    /// Returns 0 if tokens are available now.
    pub fn retry_after_secs(&self) -> f64 {
        if self.rate == 0.0 || self.tokens >= 1.0 {
            return 0.0;
        }
        let needed = 1.0 - self.tokens;
        (needed / self.rate).max(0.0)
    }

    /// REV-WARPFS-5: requests admitted since creation. Increments even in
    /// unlimited mode (where every check admits) so embedders can count
    /// total traffic, not just throttled traffic.
    pub fn admitted_count(&self) -> u64 {
        self.admitted
    }

    /// REV-WARPFS-5: requests rejected by the token bucket since creation
    /// (the same number `/metrics` reports as `rejected`). Always 0 in
    /// unlimited mode.
    pub fn rejected_count(&self) -> u64 {
        self.rejected
    }

    /// REV-WARPFS-5: current queue depth — the number of burst tokens
    /// already consumed from the bucket. Always 0 when rate limiting is
    /// disabled (`rate_limit_rps = 0`, the unlimited mode that must never
    /// warn).
    pub fn queue_depth(&self) -> u64 {
        if self.rate == 0.0 {
            return 0;
        }
        ((self.capacity as f64 - self.tokens).max(0.0)) as u64
    }

    /// REV-WARPFS-5: true when the queue depth exceeds
    /// [`BACKPRESSURE_THRESHOLD`] of the bucket capacity. Always false when
    /// rate limiting is disabled — `rate_limit_rps = 0` means unlimited and
    /// must stay warning-free (backward compatibility).
    pub fn under_backpressure(&self) -> bool {
        if self.rate == 0.0 {
            return false;
        }
        let capacity = self.capacity as f64;
        capacity - self.tokens > BACKPRESSURE_THRESHOLD * capacity
    }

    /// REV-WARPFS-5: the metrics snapshot served by the HTTP `/metrics`
    /// endpoint (`{"rate", "queue_depth", "rejected"}`). `rate` is the
    /// configured sustained rate (0 = unlimited); `rejected` counts only
    /// `-32000` token-bucket rejections.
    pub fn metrics_json(&self) -> serde_json::Value {
        serde_json::json!({
            "rate": self.rate,
            "queue_depth": self.queue_depth(),
            "rejected": self.rejected,
        })
    }

    /// Replenish tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity as f64);
        self.last_refill = now;
    }

    /// REV-WARPFS-5 (test seam, lib-internal unit tests only): pin the
    /// token count directly so tests can set queue depth deterministically
    /// — a `check()` loop races the wall-clock refill and flakes under
    /// host load. Compiled out of release builds and invisible to
    /// integration tests.
    #[cfg(test)]
    pub(crate) fn force_tokens(&mut self, tokens: f64) {
        self.tokens = tokens;
        self.last_refill = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn unlimited_when_rate_is_zero() {
        let mut rl = RateLimiter::new(0);
        for _ in 0..10_000 {
            assert!(rl.check(), "unlimited rate limiter should always allow");
        }
    }

    #[test]
    fn allows_up_to_capacity_then_blocks() {
        let mut rl = RateLimiter::new(3);
        // First 3 requests should pass
        assert!(rl.check());
        assert!(rl.check());
        assert!(rl.check());
        // 4th should block
        assert!(!rl.check());
    }

    #[test]
    fn refills_over_time() {
        let mut rl = RateLimiter::new(100); // 100 rps → 1 token per 10ms
                                            // Drain the bucket
        for _ in 0..100 {
            assert!(rl.check());
        }
        assert!(!rl.check(), "bucket should be empty");

        // Wait for 20ms → ~2 tokens
        sleep(Duration::from_millis(20));
        assert!(rl.check());
        assert!(rl.check());
        assert!(!rl.check(), "only ~2 tokens should have refilled");
    }

    #[test]
    fn retry_after_returns_positive_when_empty() {
        let mut rl = RateLimiter::new(1);
        assert!(rl.check());
        assert!(!rl.check());
        let wait = rl.retry_after_secs();
        assert!(wait > 0.0, "retry_after should be positive when empty");
        assert!(wait <= 1.0, "should not exceed 1 second at 1 rps");
    }

    #[test]
    fn retry_after_zero_when_unlimited() {
        let rl = RateLimiter::new(0);
        assert_eq!(rl.retry_after_secs(), 0.0);
    }

    #[test]
    fn new_rate_limiter_has_full_bucket() {
        let rl = RateLimiter::new(10);
        // Should allow at least the capacity right away
        assert_eq!(rl.tokens, 10.0);
        assert!(rl.retry_after_secs() == 0.0);
    }

    #[test]
    fn capacity_matches_rate() {
        let rl = RateLimiter::new(5);
        assert_eq!(rl.capacity, 5);
    }

    // ───────────── REV-WARPFS-5: backpressure + metrics ─────────────

    #[test]
    fn full_bucket_never_warns() {
        let rl = RateLimiter::new(10);
        assert!(!rl.under_backpressure(), "full bucket must not warn");
        assert_eq!(rl.queue_depth(), 0);
    }

    #[test]
    fn drained_bucket_warns() {
        let mut rl = RateLimiter::new(3);
        // Drained deterministically (in-crate test: private field). A
        // check()-loop would flake under host load — a refill can mint a
        // token mid-loop on a 50-load box.
        rl.tokens = 0.0;
        // Depth = 3/3 = 100% > 80%.
        assert!(rl.under_backpressure(), "empty bucket must warn");
        assert_eq!(rl.queue_depth(), 3);
    }

    /// The exact 80% boundary: depth == capacity * threshold does NOT warn
    /// (the signal is strictly-greater), one token deeper does.
    #[test]
    fn depth_at_threshold_boundary() {
        let mut rl = RateLimiter::new(10);
        rl.tokens = 2.0; // depth 8/10 = exactly 80%
        assert!(!rl.under_backpressure(), "80.0% is not backpressure");
        assert_eq!(rl.queue_depth(), 8);
        rl.tokens = 1.999; // depth 80.01%
        assert!(rl.under_backpressure());
    }

    #[test]
    fn depth_below_threshold_does_not_warn() {
        let mut rl = RateLimiter::new(10);
        assert!(rl.check()); // depth 1/10 = 10%
        assert!(!rl.under_backpressure());
        assert_eq!(rl.queue_depth(), 1);
    }

    #[test]
    fn unlimited_never_warns_or_counts_queue() {
        // Backward-compatibility pin: rate_limit_rps = 0 must stay
        // warning-free no matter how hard it is hammered.
        let mut rl = RateLimiter::new(0);
        for _ in 0..1_000 {
            assert!(rl.check());
            assert!(!rl.under_backpressure(), "unlimited must never warn");
            assert_eq!(rl.queue_depth(), 0, "unlimited has no queue");
        }
        assert_eq!(rl.rejected, 0);
        assert_eq!(rl.admitted, 1_000);
    }

    /// Exceeding the burst capacity rejects at least one request and puts
    /// the limiter into backpressure. Hammering 12x capacity guarantees the
    /// bucket ends EMPTY (depth = capacity = 100% > 80%) regardless of
    /// refill timing — no wall-clock assumption.
    #[test]
    fn exceeding_capacity_rejects_and_warns() {
        let mut rl = RateLimiter::new(3);
        for _ in 0..12 {
            rl.check();
        }
        assert!(
            rl.rejected >= 1,
            "requests beyond capacity must be rejected"
        );
        assert_eq!(rl.admitted + rl.rejected, 12, "every check is counted");
        assert!(
            rl.under_backpressure(),
            "a bucket drained past 80% must warn"
        );
    }

    #[test]
    fn metrics_snapshot_maps_live_state() {
        let mut rl = RateLimiter::new(10);
        for _ in 0..12 {
            rl.check();
        }
        let m = rl.metrics_json();
        assert_eq!(m["rate"], 10.0);
        assert_eq!(m["queue_depth"], rl.queue_depth());
        assert_eq!(m["rejected"], rl.rejected);
    }

    #[test]
    fn metrics_snapshot_for_unlimited_is_zeroed() {
        let mut rl = RateLimiter::new(0);
        for _ in 0..50 {
            assert!(rl.check());
        }
        let m = rl.metrics_json();
        assert_eq!(m["rate"], 0.0);
        assert_eq!(m["queue_depth"], 0);
        assert_eq!(m["rejected"], 0);
    }

    #[test]
    fn threshold_is_80_percent() {
        assert_eq!(BACKPRESSURE_THRESHOLD, 0.8);
        assert_eq!(BACKPRESSURE_WARNING, "high queue depth");
    }
}
