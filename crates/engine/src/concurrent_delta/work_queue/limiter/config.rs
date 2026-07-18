//! Builder for [`AimdLimiter`] configuration.
//!
//! `LimiterConfig` validates and clamps construction parameters before
//! producing an `AimdLimiter`. Defaults match the design RFC: alpha=1,
//! beta=1/2, min_limit=1, max_limit=8x initial_target.

use super::AimdLimiter;

/// Builder-pattern configuration for [`AimdLimiter`].
///
/// Construct with [`LimiterConfig::new`], then chain setters and call
/// [`LimiterConfig::build`] to produce an `AimdLimiter`. Sensible defaults
/// match the design RFC: alpha=1, beta=1/2, min_limit=1,
/// max_limit=8x initial_target.
#[derive(Debug, Clone)]
#[must_use]
pub struct LimiterConfig {
    pub(super) initial_target: usize,
    pub(super) min_limit: usize,
    pub(super) max_limit: usize,
    pub(super) alpha: u32,
    pub(super) beta_num: u32,
    pub(super) beta_den: u32,
}

impl LimiterConfig {
    /// Returns a new config with sensible defaults for an `initial_target`.
    ///
    /// Defaults: `min_limit = 1`, `max_limit = 8 * initial_target.max(1)`,
    /// `alpha = 1`, `beta_num = 1`, `beta_den = 2` (multiplicative decrease 0.5).
    pub fn new(initial_target: usize) -> Self {
        let initial_target = initial_target.max(1);
        Self {
            initial_target,
            min_limit: 1,
            max_limit: initial_target.saturating_mul(8).max(initial_target),
            alpha: 1,
            beta_num: 1,
            beta_den: 2,
        }
    }

    /// Sets the floor `target` may decrease to. Clamped to at least 1.
    pub fn min_limit(mut self, min_limit: usize) -> Self {
        self.min_limit = min_limit.max(1);
        self
    }

    /// Sets the ceiling `target` may increase to.
    pub fn max_limit(mut self, max_limit: usize) -> Self {
        self.max_limit = max_limit.max(1);
        self
    }

    /// Sets the additive-increase step. Defaults to 1 (one slot per window).
    pub fn alpha(mut self, alpha: u32) -> Self {
        self.alpha = alpha.max(1);
        self
    }

    /// Sets the multiplicative-decrease ratio numerator.
    pub fn beta_num(mut self, beta_num: u32) -> Self {
        self.beta_num = beta_num.max(1);
        self
    }

    /// Sets the multiplicative-decrease ratio denominator. Must be > `beta_num`
    /// for the ratio to actually shrink `target`.
    pub fn beta_den(mut self, beta_den: u32) -> Self {
        self.beta_den = beta_den.max(2);
        self
    }

    /// Builds an [`AimdLimiter`] applying clamps so that `min_limit <= initial_target <= max_limit`.
    pub fn build(self) -> AimdLimiter {
        AimdLimiter::new(self)
    }
}
