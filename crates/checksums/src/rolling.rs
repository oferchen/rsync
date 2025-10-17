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
    /// The number of outgoing bytes does not match the number of incoming bytes.
    MismatchedSliceLength {
        /// Number of bytes being removed from the rolling window.
        outgoing: usize,
        /// Number of bytes being appended to the rolling window.
        incoming: usize,
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
            Self::MismatchedSliceLength { outgoing, incoming } => write!(
                f,
                "rolling checksum requires outgoing ({outgoing}) and incoming ({incoming}) slices to have the same length"
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

    /// Returns the current window length as a 32-bit value while validating invariants.
    fn window_len_u32(&self) -> Result<u32, RollingError> {
        if self.len == 0 {
            return Err(RollingError::EmptyWindow);
        }

        u32::try_from(self.len).map_err(|_| RollingError::WindowTooLarge { len: self.len })
    }

    /// Performs the rolling checksum update by removing `outgoing` and appending `incoming`.
    ///
    /// # Errors
    ///
    /// Returns [`RollingError::EmptyWindow`] if the checksum has not been initialised with a
    /// block and [`RollingError::WindowTooLarge`] when the window length exceeds what the
    /// upstream algorithm supports (32 bits).
    pub fn roll(&mut self, outgoing: u8, incoming: u8) -> Result<(), RollingError> {
        let window_len = self.window_len_u32()?;

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

    /// Rolls the checksum forward by replacing multiple bytes at once.
    ///
    /// The method behaves as if [`roll`](Self::roll) were called repeatedly for each pair of
    /// outgoing and incoming bytes. Providing slices of different lengths is rejected to avoid
    /// ambiguous state. Passing empty slices is allowed and leaves the checksum unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`RollingError::MismatchedSliceLength`] when the outgoing and incoming slices
    /// differ in length, [`RollingError::EmptyWindow`] if the checksum has not been seeded with a
    /// block yet, and [`RollingError::WindowTooLarge`] if the internal window length exceeds the
    /// upstream limit.
    pub fn roll_many(&mut self, outgoing: &[u8], incoming: &[u8]) -> Result<(), RollingError> {
        if outgoing.len() != incoming.len() {
            return Err(RollingError::MismatchedSliceLength {
                outgoing: outgoing.len(),
                incoming: incoming.len(),
            });
        }

        if outgoing.is_empty() {
            return Ok(());
        }

        let window_len = self.window_len_u32()?;

        let mut s1 = self.s1;
        let mut s2 = self.s2;

        for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
            let out = u32::from(out);
            let inn = u32::from(inn);

            s1 = s1.wrapping_sub(out).wrapping_add(inn) & 0xffff;
            s2 = s2
                .wrapping_sub(window_len.wrapping_mul(out))
                .wrapping_add(s1)
                & 0xffff;
        }

        self.s1 = s1;
        self.s2 = s2;

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

    /// Constructs a digest from the packed 32-bit representation used on the wire.
    ///
    /// Upstream rsync transmits the rolling checksum as two 16-bit components (`s1`
    /// and `s2`) packed into a 32-bit integer. Higher layers often know the block
    /// length separately, so the caller provides it explicitly to avoid guessing.
    /// The helper mirrors [`Self::value`], making it cheap to round-trip digests
    /// through their network encoding without manually extracting bit fields.
    #[must_use]
    pub const fn from_value(value: u32, len: usize) -> Self {
        Self {
            s1: value as u16,
            s2: (value >> 16) as u16,
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

    /// Returns whether the digest was computed from zero bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
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

    fn roll_many_sequences() -> impl Strategy<Value = (Vec<u8>, Vec<(u8, u8)>)> {
        prop::collection::vec(any::<u8>(), 1..=64).prop_flat_map(|seed| {
            let seed_clone = seed.clone();
            prop::collection::vec((any::<u8>(), any::<u8>()), 0..=32)
                .prop_map(move |pairs| (seed_clone.clone(), pairs))
        })
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
    fn digest_round_trips_through_packed_value() {
        let sample = RollingDigest::new(0x1357, 0x2468, 4096);
        let packed = sample.value();
        let unpacked = RollingDigest::from_value(packed, sample.len());

        assert_eq!(unpacked, sample);
        assert_eq!(unpacked.value(), packed);
        assert_eq!(unpacked.sum1(), sample.sum1());
        assert_eq!(unpacked.sum2(), sample.sum2());
        assert_eq!(unpacked.len(), sample.len());
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
    fn roll_many_errors_for_empty_window() {
        let mut checksum = RollingChecksum::new();
        let err = checksum
            .roll_many(b"a", b"b")
            .expect_err("rolling on empty window must fail");
        assert_eq!(err, RollingError::EmptyWindow);
        assert_eq!(checksum.digest(), RollingDigest::new(0, 0, 0));
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

    #[test]
    fn roll_many_errors_for_window_exceeding_u32() {
        let mut checksum = RollingChecksum::new();
        checksum.s1 = 1;
        checksum.s2 = 1;
        checksum.len = (u32::MAX as usize) + 1;

        let original = checksum.clone();
        let err = checksum
            .roll_many(b"a", b"b")
            .expect_err("oversized window must fail");
        assert!(matches!(err, RollingError::WindowTooLarge { .. }));
        assert_eq!(checksum, original);
    }

    #[test]
    fn roll_many_matches_multiple_single_rolls() {
        let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.";
        let window = 12;
        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window]);

        let mut reference = rolling.clone();
        let mut position = window;

        while position < data.len() {
            let advance = (data.len() - position).min(3);
            let outgoing_start = position - window;
            let outgoing_end = outgoing_start + advance;
            let incoming_end = position + advance;

            rolling
                .roll_many(
                    &data[outgoing_start..outgoing_end],
                    &data[position..incoming_end],
                )
                .expect("multi-byte roll succeeds");

            for (&out, &inn) in data[outgoing_start..outgoing_end]
                .iter()
                .zip(data[position..incoming_end].iter())
            {
                reference.roll(out, inn).expect("single roll succeeds");
            }

            assert_eq!(rolling.digest(), reference.digest());
            assert_eq!(rolling.value(), reference.value());

            position += advance;
        }
    }

    #[test]
    fn roll_many_rejects_mismatched_lengths() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"abcd");

        let err = checksum
            .roll_many(b"ab", b"c")
            .expect_err("length mismatch must fail");
        assert!(matches!(
            err,
            RollingError::MismatchedSliceLength {
                outgoing: 2,
                incoming: 1,
            }
        ));
    }

    #[test]
    fn roll_many_allows_empty_slices() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"rsync");

        checksum
            .roll_many(&[], &[])
            .expect("empty slices should be ignored");
        assert_eq!(
            checksum.digest(),
            RollingDigest::new(checksum.s1 as u16, checksum.s2 as u16, checksum.len)
        );
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

        #[test]
        fn roll_many_matches_single_rolls_for_random_sequences(
            (seed, pairs) in roll_many_sequences(),
        ) {
            let mut optimized = RollingChecksum::new();
            optimized.update(&seed);

            let mut reference = optimized.clone();

            let (outgoing, incoming): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
            optimized
                .roll_many(&outgoing, &incoming)
                .expect("multi-byte roll succeeds");

            for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
                reference
                    .roll(out, inn)
                    .expect("single-byte roll succeeds");
            }

            prop_assert_eq!(optimized.digest(), reference.digest());
            prop_assert_eq!(optimized.value(), reference.value());
        }
    }
}
