//! Token-bucket rate limiter for the MCP server.
//!
//! A configurable rate limiter that enforces a maximum requests-per-second
//! rate. When the limit is exceeded, the limiter returns the number of
//! seconds to wait before retrying.

use std::time::Instant;

#[cfg(test)]
use std::time::Duration;

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
        }
    }

    /// Check if a request is allowed. Returns `true` if allowed,
    /// `false` if rate-limited (caller should wait).
    ///
    /// Each successful check consumes one token. Tokens are
    /// replenished automatically based on elapsed time.
    pub fn check(&mut self) -> bool {
        if self.rate == 0.0 {
            return true; // Rate limiting disabled
        }
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
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

    /// Replenish tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity as f64);
        self.last_refill = now;
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
}
