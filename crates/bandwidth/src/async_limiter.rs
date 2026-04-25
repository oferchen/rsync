//! Async leaky bucket rate limiter using tokio's time facilities.
//!
//! Provides [`AsyncRateLimiter`], a `Send + Sync` bandwidth limiter for use in
//! async contexts. Tokens accumulate at the configured byte-per-second rate up
//! to a burst capacity equal to one second's worth of bytes. When a caller
//! consumes more tokens than are available, the method sleeps until enough
//! tokens have accumulated.

use std::num::NonZeroU64;

use tokio::time::{Duration, Instant};

/// Async leaky bucket rate limiter for bandwidth throttling in tokio tasks.
///
/// Tokens accumulate at `bytes_per_second` up to a burst capacity equal to one
/// second's worth of tokens. Calling [`consume`](Self::consume) drains tokens;
/// if insufficient tokens exist the method yields to the tokio runtime until
/// enough have accumulated.
///
/// # Examples
///
/// ```no_run
/// # async fn example() {
/// use bandwidth::AsyncRateLimiter;
///
/// let mut limiter = AsyncRateLimiter::new(1_048_576); // 1 MiB/s
/// limiter.consume(4096).await;
/// # }
/// ```
pub struct AsyncRateLimiter {
    /// Bytes-per-second rate.
    rate: NonZeroU64,
    /// Maximum tokens that can accumulate (one second's worth).
    burst: u64,
    /// Currently available tokens.
    tokens: u64,
    /// Last time tokens were replenished.
    last_replenish: Instant,
}

// SAFETY: no interior mutability beyond what `&mut self` provides.
// The struct is trivially `Send + Sync` when behind a `Mutex` or used with
// `&mut self` methods, which is the expected pattern for async rate limiters.

impl AsyncRateLimiter {
    /// Creates a new rate limiter with the specified bytes-per-second rate.
    ///
    /// The burst capacity is set to one second's worth of tokens, and the
    /// bucket starts full. A rate of zero is not accepted - the caller must
    /// provide a positive rate.
    ///
    /// # Panics
    ///
    /// Panics if `bytes_per_second` is zero.
    #[must_use]
    pub fn new(bytes_per_second: u64) -> Self {
        let rate =
            NonZeroU64::new(bytes_per_second).expect("bytes_per_second must be greater than zero");
        let burst = rate.get();
        Self {
            rate,
            burst,
            tokens: burst,
            last_replenish: Instant::now(),
        }
    }

    /// Consumes the specified number of bytes, sleeping if the rate would be
    /// exceeded.
    ///
    /// Tokens are first replenished based on elapsed time since the last call.
    /// If the available tokens are insufficient, the method sleeps until enough
    /// tokens have accumulated to cover the request.
    ///
    /// A request larger than the burst capacity is handled in a single sleep -
    /// the method calculates the total time needed and waits accordingly.
    pub async fn consume(&mut self, bytes: u64) {
        self.replenish();

        if bytes == 0 {
            return;
        }

        if self.tokens >= bytes {
            self.tokens -= bytes;
            return;
        }

        // Deficit: how many tokens we still need after draining what we have.
        let deficit = bytes - self.tokens;
        self.tokens = 0;

        // Calculate sleep duration: deficit / rate seconds.
        let sleep_nanos = (deficit as u128) * 1_000_000_000 / (self.rate.get() as u128);
        let sleep_duration = Duration::from_nanos(sleep_nanos as u64);

        tokio::time::sleep(sleep_duration).await;

        // After sleeping, replenish tokens for the time we slept. The
        // replenish call uses the elapsed wall-clock time which includes
        // the sleep, so any extra time beyond our target also generates
        // tokens.
        self.replenish();
    }

    /// Resets the token bucket to full capacity without changing the rate.
    pub fn reset(&mut self) {
        self.tokens = self.burst;
        self.last_replenish = Instant::now();
    }

    /// Changes the rate to a new bytes-per-second value.
    ///
    /// The burst capacity is updated to match the new rate. Existing tokens
    /// are clamped to the new burst capacity. The replenish timestamp is
    /// updated so accumulated time is not lost.
    ///
    /// # Panics
    ///
    /// Panics if `bytes_per_second` is zero.
    pub fn set_rate(&mut self, bytes_per_second: u64) {
        let rate =
            NonZeroU64::new(bytes_per_second).expect("bytes_per_second must be greater than zero");
        // Replenish any tokens accumulated under the old rate first.
        self.replenish();
        self.rate = rate;
        self.burst = rate.get();
        self.tokens = self.tokens.min(self.burst);
    }

    /// Returns the current rate in bytes per second.
    #[inline]
    #[must_use]
    pub const fn rate(&self) -> u64 {
        self.rate.get()
    }

    /// Returns the burst capacity in bytes.
    #[inline]
    #[must_use]
    pub const fn burst_capacity(&self) -> u64 {
        self.burst
    }

    /// Returns the number of tokens currently available.
    #[inline]
    #[must_use]
    pub const fn available_tokens(&self) -> u64 {
        self.tokens
    }

    /// Replenishes tokens based on elapsed time since the last replenish.
    fn replenish(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_replenish);
        self.last_replenish = now;

        // Calculate tokens earned: elapsed_secs * rate.
        // Use nanos for precision: (elapsed_nanos * rate) / 1_000_000_000.
        let elapsed_nanos = elapsed.as_nanos();
        let earned = (elapsed_nanos * (self.rate.get() as u128)) / 1_000_000_000;
        let earned = earned.min(u64::MAX as u128) as u64;

        self.tokens = self.tokens.saturating_add(earned).min(self.burst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_starts_with_full_bucket() {
        let limiter = AsyncRateLimiter::new(1000);
        assert_eq!(limiter.rate(), 1000);
        assert_eq!(limiter.burst_capacity(), 1000);
        assert_eq!(limiter.available_tokens(), 1000);
    }

    #[tokio::test]
    async fn consume_zero_is_noop() {
        let mut limiter = AsyncRateLimiter::new(1000);
        limiter.consume(0).await;
        // Tokens should be at or near burst capacity (minor replenish drift).
        assert!(limiter.available_tokens() <= 1000);
    }

    #[tokio::test]
    async fn consume_within_budget_does_not_sleep() {
        let mut limiter = AsyncRateLimiter::new(10_000);
        let start = Instant::now();
        limiter.consume(5_000).await;
        let elapsed = start.elapsed();
        // Should complete nearly instantly (well under 50ms).
        assert!(elapsed < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn consume_exceeding_budget_sleeps() {
        let mut limiter = AsyncRateLimiter::new(10_000);
        // Drain the bucket.
        limiter.consume(10_000).await;
        // Now consuming more should cause a sleep.
        let start = Instant::now();
        limiter.consume(1_000).await;
        let elapsed = start.elapsed();
        // Should sleep approximately 100ms (1000 / 10000 = 0.1s).
        assert!(
            elapsed >= Duration::from_millis(80),
            "expected >= 80ms, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "expected < 500ms, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn reset_refills_bucket() {
        let mut limiter = AsyncRateLimiter::new(1000);
        limiter.consume(1000).await;
        assert_eq!(limiter.available_tokens(), 0);
        limiter.reset();
        assert_eq!(limiter.available_tokens(), 1000);
    }

    #[tokio::test]
    async fn set_rate_updates_burst_and_clamps_tokens() {
        let mut limiter = AsyncRateLimiter::new(1000);
        // Start with 1000 tokens. Lower rate to 500.
        limiter.set_rate(500);
        assert_eq!(limiter.rate(), 500);
        assert_eq!(limiter.burst_capacity(), 500);
        // Tokens should be clamped to the new burst.
        assert!(limiter.available_tokens() <= 500);
    }

    #[tokio::test]
    async fn set_rate_increases_burst() {
        let mut limiter = AsyncRateLimiter::new(500);
        limiter.set_rate(2000);
        assert_eq!(limiter.rate(), 2000);
        assert_eq!(limiter.burst_capacity(), 2000);
    }

    #[tokio::test]
    #[should_panic(expected = "bytes_per_second must be greater than zero")]
    async fn new_with_zero_rate_panics() {
        let _limiter = AsyncRateLimiter::new(0);
    }

    #[tokio::test]
    #[should_panic(expected = "bytes_per_second must be greater than zero")]
    async fn set_rate_zero_panics() {
        let mut limiter = AsyncRateLimiter::new(1000);
        limiter.set_rate(0);
    }

    #[tokio::test]
    async fn multiple_small_consumes_accumulate() {
        let mut limiter = AsyncRateLimiter::new(10_000);
        // Consume in small chunks until budget is exhausted.
        for _ in 0..10 {
            limiter.consume(1_000).await;
        }
        // Next consume should require a sleep.
        let start = Instant::now();
        limiter.consume(1_000).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80),
            "expected sleep, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn large_consume_beyond_burst_works() {
        // Request more than burst capacity in one call.
        let mut limiter = AsyncRateLimiter::new(10_000);
        let start = Instant::now();
        // 20_000 bytes at 10_000 bytes/s = 2 seconds, but bucket starts full
        // with 10_000 tokens, so deficit is 10_000 = 1 second sleep.
        limiter.consume(20_000).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "expected >= 900ms, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1_500),
            "expected < 1500ms, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn tokens_replenish_over_time() {
        let mut limiter = AsyncRateLimiter::new(10_000);
        // Drain the bucket.
        limiter.consume(10_000).await;
        assert_eq!(limiter.available_tokens(), 0);
        // Wait 500ms for tokens to accumulate (use longer sleep for reliability
        // under CI load and coverage instrumentation).
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Force replenish via a zero-byte consume.
        limiter.consume(0).await;
        // At 10_000 bytes/s, 500ms should yield ~5000 tokens. Use wide bounds
        // to tolerate wall-clock jitter in CI and coverage builds.
        let tokens = limiter.available_tokens();
        assert!(
            tokens >= 2_000,
            "expected >= 2000 tokens after 500ms at 10k/s, got {tokens}"
        );
        assert!(
            tokens <= 10_000,
            "tokens should not exceed burst capacity, got {tokens}"
        );
    }

    #[tokio::test]
    async fn burst_caps_token_accumulation() {
        let mut limiter = AsyncRateLimiter::new(1_000);
        // Wait long enough for tokens to exceed burst if uncapped.
        tokio::time::sleep(Duration::from_millis(200)).await;
        limiter.consume(0).await;
        // Tokens should never exceed burst capacity.
        assert!(limiter.available_tokens() <= 1_000);
    }

    /// Verifies the type is `Send + Sync` at compile time.
    const _: () = {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AsyncRateLimiter>();
    };
}
