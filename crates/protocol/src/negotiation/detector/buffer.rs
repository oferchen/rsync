//! Buffer access and lifecycle management for the prefix accumulator.

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;
use crate::negotiation::BufferedPrefixTooSmall;

use super::NegotiationPrologueDetector;

impl NegotiationPrologueDetector {
    /// Returns the prefix bytes buffered while deciding on the negotiation style.
    ///
    /// When the detector concludes that the peer is using the legacy ASCII
    /// greeting, the already consumed bytes must be included when parsing the
    /// full banner. Upstream rsync accomplishes this by reusing the peeked
    /// prefix. The buffer grows across subsequent [`Self::observe`] calls until
    /// the canonical `@RSYNCD:` prefix has been captured or a mismatch forces
    /// the legacy classification. For binary negotiations, no bytes are retained
    /// and this method returns an empty slice.
    #[must_use]
    #[inline]
    pub fn buffered_prefix(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Copies the buffered prefix into the caller-provided slice.
    ///
    /// Higher layers that reuse stack-allocated scratch space can avoid
    /// temporary vectors by copying the buffered prefix into an existing slice.
    /// When the destination slice is too small, a [`BufferedPrefixTooSmall`]
    /// error is returned and no data is written.
    pub fn copy_buffered_prefix_into(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        let required = self.len;

        if target.len() < required {
            return Err(BufferedPrefixTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffer[..required]);
        Ok(required)
    }

    /// Copies the buffered prefix into a caller-provided array without allocation.
    ///
    /// Convenience wrapper over
    /// [`copy_buffered_prefix_into`](Self::copy_buffered_prefix_into) that
    /// accepts a fixed-size array directly. When the array cannot hold the
    /// buffered prefix a [`BufferedPrefixTooSmall`] error is returned and no
    /// bytes are written.
    pub fn copy_buffered_prefix_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        self.copy_buffered_prefix_into(target.as_mut_slice())
    }

    /// Returns the number of bytes retained in the prefix buffer.
    ///
    /// The detector only stores bytes while determining whether the exchange
    /// uses the legacy ASCII greeting. Once the binary path has been selected
    /// the buffer remains empty.
    #[must_use]
    #[inline]
    pub const fn buffered_len(&self) -> usize {
        self.len
    }

    /// Resets the detector to its initial state so it can be reused for a new
    /// connection attempt.
    ///
    /// Restores the struct to the state produced by
    /// [`NegotiationPrologueDetector::new`], mirroring upstream rsync's practice
    /// of zeroing its detection state before accepting another connection.
    pub const fn reset(&mut self) {
        self.buffer = [0; LEGACY_DAEMON_PREFIX_LEN];
        self.len = 0;
        self.decided = None;
        self.prefix_complete = false;
    }
}
