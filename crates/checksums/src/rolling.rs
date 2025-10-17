use core::fmt;

/// Errors that can occur while updating the rolling checksum state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RollingError {
    /// The checksum window is empty, preventing the rolling update from making progress.
    EmptyWindow,
    /// The checksum window length exceeds what can be represented in 32 bits.
    WindowTooLarge {
        /// Number of bytes present in the rolling window when the error was raised.
        len: usize,
    },
}

impl fmt::Display for RollingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWindow => write!(f, "rolling checksum requires a non-empty window"),
            Self::WindowTooLarge { len } => write!(
                f,
                "rolling checksum window of {len} bytes exceeds 32-bit limit"
            ),
        }
    }
}

impl std::error::Error for RollingError {}

/// Rolling checksum used by rsync for weak block matching (often called `rsum`).
///
/// The checksum mirrors upstream rsync's Adler-32 variant where the first component
/// (`s1`) accumulates the byte sum and the second component (`s2`) tracks the sum of
/// the running prefix sums. Both components are truncated to 16 bits after every
/// update to match the canonical algorithm used during delta transfer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RollingChecksum {
    s1: u32,
    s2: u32,
    len: usize,
}

impl RollingChecksum {
    /// Creates a new rolling checksum with zeroed state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            s1: 0,
            s2: 0,
            len: 0,
        }
    }

    /// Resets the checksum back to its initial state.
    pub fn reset(&mut self) {
        self.s1 = 0;
        self.s2 = 0;
        self.len = 0;
    }

    /// Returns the number of bytes that contributed to the current state.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no bytes have been observed yet.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Updates the checksum with an additional slice of bytes.
    pub fn update(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        let mut s1 = self.s1;
        let mut s2 = self.s2;

        let mut iter = chunk.chunks_exact(4);
        for block in &mut iter {
            s1 = s1.wrapping_add(u32::from(block[0]));
            s2 = s2.wrapping_add(s1);

            s1 = s1.wrapping_add(u32::from(block[1]));
            s2 = s2.wrapping_add(s1);

            s1 = s1.wrapping_add(u32::from(block[2]));
            s2 = s2.wrapping_add(s1);

            s1 = s1.wrapping_add(u32::from(block[3]));
            s2 = s2.wrapping_add(s1);
        }

        for &byte in iter.remainder() {
            s1 = s1.wrapping_add(u32::from(byte));
            s2 = s2.wrapping_add(s1);
        }

        self.s1 = s1 & 0xffff;
        self.s2 = s2 & 0xffff;
        self.len = self.len.saturating_add(chunk.len());
    }

    /// Updates the checksum by recomputing the state for a fresh block.
    ///
    /// This helper clears the internal state before delegating to [`update`](Self::update),
    /// making it convenient to compute the checksum of a block without manually calling
    /// [`reset`](Self::reset).
    pub fn update_from_block(&mut self, block: &[u8]) {
        self.reset();
        self.update(block);
    }

    /// Performs the rolling checksum update by removing `outgoing` and appending `incoming`.
    ///
    /// # Errors
    ///
    /// Returns [`RollingError::EmptyWindow`] if the checksum has not been initialised with a
    /// block and [`RollingError::WindowTooLarge`] when the window length exceeds what the
    /// upstream algorithm supports (32 bits).
    pub fn roll(&mut self, outgoing: u8, incoming: u8) -> Result<(), RollingError> {
        if self.len == 0 {
            return Err(RollingError::EmptyWindow);
        }

        let window_len =
            u32::try_from(self.len).map_err(|_| RollingError::WindowTooLarge { len: self.len })?;

        let out = u32::from(outgoing);
        let inn = u32::from(incoming);

        let new_s1 = self.s1.wrapping_sub(out).wrapping_add(inn) & 0xffff;
        let new_s2 = self
            .s2
            .wrapping_sub(window_len.wrapping_mul(out))
            .wrapping_add(new_s1)
            & 0xffff;

        self.s1 = new_s1;
        self.s2 = new_s2;
        Ok(())
    }

    /// Returns the rolling checksum value in rsync's packed 32-bit representation.
    #[must_use]
    pub const fn value(&self) -> u32 {
        (self.s2 << 16) | self.s1
    }

    /// Returns the current state as a structured digest.
    #[must_use]
    pub fn digest(&self) -> RollingDigest {
        RollingDigest {
            s1: self.s1 as u16,
            s2: self.s2 as u16,
            len: self.len,
        }
    }
}

/// Digest produced by the rolling checksum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollingDigest {
    s1: u16,
    s2: u16,
    len: usize,
}

impl RollingDigest {
    /// Creates a digest from individual components.
    #[must_use]
    pub const fn new(sum1: u16, sum2: u16, len: usize) -> Self {
        Self {
            s1: sum1,
            s2: sum2,
            len,
        }
    }

    /// Returns the first checksum component (sum of bytes).
    #[must_use]
    pub const fn sum1(&self) -> u16 {
        self.s1
    }

    /// Returns the second checksum component (sum of prefix sums).
    #[must_use]
    pub const fn sum2(&self) -> u16 {
        self.s2
    }

    /// Returns the number of bytes that contributed to the digest.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns the checksum in rsync's packed 32-bit representation.
    #[must_use]
    pub const fn value(&self) -> u32 {
        ((self.s2 as u32) << 16) | (self.s1 as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    fn reference_digest(data: &[u8]) -> RollingDigest {
        let mut s1: u64 = 0;
        let mut s2: u64 = 0;

        for &byte in data {
            s1 += u64::from(byte);
            s2 += s1;
        }

        RollingDigest::new((s1 & 0xffff) as u16, (s2 & 0xffff) as u16, data.len())
    }

    fn random_data_and_window() -> impl Strategy<Value = (Vec<u8>, usize)> {
        prop::collection::vec(any::<u8>(), 1..=256).prop_flat_map(|data| {
            let len = data.len();
            let window_range = 1..=len;
            (Just(data), window_range).prop_map(|(data, window)| (data, window))
        })
    }

    fn chunked_sequences() -> impl Strategy<Value = Vec<Vec<u8>>> {
        prop::collection::vec(prop::collection::vec(any::<u8>(), 0..=64), 1..=8)
    }

    #[test]
    fn digest_matches_reference_for_known_input() {
        let data = b"rsync rolling checksum";
        let digest = reference_digest(data);

        let mut checksum = RollingChecksum::new();
        checksum.update(data);
        assert_eq!(checksum.digest(), digest);
        assert_eq!(checksum.value(), digest.value());
    }

    #[test]
    fn recomputing_block_yields_same_state() {
        let data = b"0123456789abcdef";

        let mut checksum = RollingChecksum::new();
        checksum.update(&data[..8]);

        let mut recomputed = RollingChecksum::new();
        recomputed.update_from_block(&data[..8]);

        assert_eq!(checksum.digest(), recomputed.digest());
    }

    #[test]
    fn rolling_matches_recomputed_checksum() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let window = 12;

        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window]);

        for start in 1..=data.len() - window {
            let outgoing = data[start - 1];
            let incoming = data[start + window - 1];
            rolling.roll(outgoing, incoming).expect("rolling succeeds");

            let mut expected = RollingChecksum::new();
            expected.update(&data[start..start + window]);
            assert_eq!(rolling.digest(), expected.digest());
        }
    }

    #[test]
    fn roll_errors_for_empty_window() {
        let mut checksum = RollingChecksum::new();
        let err = checksum
            .roll(0, 0)
            .expect_err("rolling on empty window must fail");
        assert_eq!(err, RollingError::EmptyWindow);
    }

    #[test]
    fn roll_errors_for_window_exceeding_u32() {
        let mut checksum = RollingChecksum::new();
        checksum.s1 = 1;
        checksum.s2 = 1;
        checksum.len = (u32::MAX as usize) + 1;

        let err = checksum.roll(0, 0).expect_err("oversized window must fail");
        assert!(matches!(err, RollingError::WindowTooLarge { .. }));
    }

    proptest! {
        #[test]
        fn rolling_update_matches_single_pass(chunks in chunked_sequences()) {
            let mut incremental = RollingChecksum::new();
            let mut concatenated = Vec::new();

            for chunk in &chunks {
                incremental.update(chunk);
                concatenated.extend_from_slice(chunk);
            }

            let mut single_pass = RollingChecksum::new();
            single_pass.update(&concatenated);

            prop_assert_eq!(incremental.digest(), single_pass.digest());
            prop_assert_eq!(incremental.value(), single_pass.value());
        }

        #[test]
        fn rolling_matches_reference_for_random_windows((data, window) in random_data_and_window()) {
            let mut rolling = RollingChecksum::new();
            rolling.update(&data[..window]);

            let mut reference = RollingChecksum::new();
            reference.update(&data[..window]);

            prop_assert_eq!(rolling.digest(), reference.digest());
            prop_assert_eq!(rolling.value(), reference.value());

            if data.len() > window {
                for start in 1..=data.len() - window {
                    let outgoing = data[start - 1];
                    let incoming = data[start + window - 1];
                    rolling
                        .roll(outgoing, incoming)
                        .expect("rolling update must succeed");

                    let mut recomputed = RollingChecksum::new();
                    recomputed.update(&data[start..start + window]);

                    prop_assert_eq!(rolling.digest(), recomputed.digest());
                    prop_assert_eq!(rolling.value(), recomputed.value());
                }
            }
        }
    }
}
