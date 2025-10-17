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

/// Error returned when reconstructing a rolling checksum digest from a byte slice of the wrong length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollingSliceError {
    len: usize,
}

impl RollingSliceError {
    /// Number of bytes the caller supplied when the error was raised.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Reports whether the provided slice was empty when the error occurred.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingDigest, RollingSliceError};
    ///
    /// let err = RollingDigest::from_le_slice(&[], 0).unwrap_err();
    /// assert!(err.is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Number of bytes required to decode a rolling checksum digest.
    pub const EXPECTED_LEN: usize = 4;
}

impl fmt::Display for RollingSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rolling checksum digest requires {} bytes, received {}",
            Self::EXPECTED_LEN,
            self.len
        )
    }
}

impl std::error::Error for RollingSliceError {}

/// Rolling checksum used by rsync for weak block matching (often called `rsum`).
///
/// The checksum mirrors upstream rsync's Adler-32 variant where the first component
/// (`s1`) accumulates the byte sum and the second component (`s2`) tracks the sum of
/// the running prefix sums. Both components are truncated to 16 bits after every
/// update to match the canonical algorithm used during delta transfer.
#[doc(alias = "rsum")]
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

    /// Reconstructs a rolling checksum from a previously captured digest.
    ///
    /// The helper mirrors the restoration logic used by upstream rsync when a receiver
    /// rehydrates the checksum state from the `sum1`/`sum2` pair transmitted over the
    /// wire. Providing a dedicated constructor avoids repeating the field mapping in
    /// higher layers and keeps the internal truncation rules encapsulated within the
    /// type. The returned checksum is immediately ready for further rolling updates.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingChecksum, RollingDigest};
    ///
    /// let mut checksum = RollingChecksum::new();
    /// checksum.update(b"delta state");
    /// let digest = checksum.digest();
    ///
    /// let restored = RollingChecksum::from_digest(digest);
    /// assert_eq!(restored.digest(), digest);
    /// ```
    #[must_use]
    pub const fn from_digest(digest: RollingDigest) -> Self {
        Self {
            s1: digest.sum1() as u32,
            s2: digest.sum2() as u32,
            len: digest.len(),
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

impl From<RollingDigest> for RollingChecksum {
    /// Converts a [`RollingDigest`] back into a [`RollingChecksum`] state.
    fn from(digest: RollingDigest) -> Self {
        Self::from_digest(digest)
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

    /// Constructs a digest from the little-endian byte representation used by upstream rsync.
    ///
    /// Upstream serialises the rolling checksum with the `SIVAL` macro, which stores the packed
    /// value as little-endian bytes on the wire. Parsing the checksum therefore requires decoding
    /// the payload in the same order before recovering the logical components. This helper mirrors
    /// [`Self::from_value`] while avoiding an intermediate [`u32`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingDigest;
    ///
    /// let digest = RollingDigest::new(0x1357, 0x2468, 4096);
    /// let bytes = digest.to_le_bytes();
    /// let parsed = RollingDigest::from_le_bytes(bytes, digest.len());
    ///
    /// assert_eq!(parsed, digest);
    /// assert_eq!(parsed.sum1(), 0x1357);
    /// assert_eq!(parsed.sum2(), 0x2468);
    /// ```
    #[doc(alias = "SIVAL")]
    #[must_use]
    pub const fn from_le_bytes(bytes: [u8; 4], len: usize) -> Self {
        Self::from_value(u32::from_le_bytes(bytes), len)
    }

    /// Constructs a digest from a little-endian byte slice, validating the input length.
    ///
    /// This helper complements [`Self::from_le_bytes`] by accepting arbitrary byte slices, making
    /// it convenient to decode digests from network buffers without first converting them into an
    /// array. When the slice does not contain exactly four bytes, the function returns
    /// [`RollingSliceError`], mirroring upstream rsync which treats truncated digests as fatal
    /// protocol violations.
    ///
    /// # Errors
    ///
    /// Returns [`RollingSliceError`] if `bytes` does not contain exactly four elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingDigest, RollingSliceError};
    ///
    /// let digest = RollingDigest::new(0x1357, 0x2468, 4096);
    /// let parsed = RollingDigest::from_le_slice(&digest.to_le_bytes(), digest.len())?;
    /// assert_eq!(parsed.sum1(), 0x1357);
    /// assert_eq!(parsed.sum2(), 0x2468);
    /// # Ok::<(), RollingSliceError>(())
    /// ```
    pub fn from_le_slice(bytes: &[u8], len: usize) -> Result<Self, RollingSliceError> {
        if bytes.len() != RollingSliceError::EXPECTED_LEN {
            return Err(RollingSliceError { len: bytes.len() });
        }

        let mut array = [0u8; RollingSliceError::EXPECTED_LEN];
        array.copy_from_slice(bytes);
        Ok(Self::from_le_bytes(array, len))
    }

    /// Returns the first checksum component (sum of bytes).
    #[doc(alias = "s1")]
    #[must_use]
    pub const fn sum1(&self) -> u16 {
        self.s1
    }

    /// Returns the second checksum component (sum of prefix sums).
    #[doc(alias = "s2")]
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

    /// Returns the checksum encoded as the little-endian byte sequence used on the wire.
    #[doc(alias = "SIVAL")]
    #[must_use]
    pub const fn to_le_bytes(&self) -> [u8; 4] {
        self.value().to_le_bytes()
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
    fn digest_round_trips_through_le_bytes() {
        let sample = RollingDigest::new(0xabcd, 0x1234, 512);
        let bytes = sample.to_le_bytes();
        let parsed = RollingDigest::from_le_bytes(bytes, sample.len());

        assert_eq!(parsed, sample);
        assert_eq!(parsed.to_le_bytes(), bytes);
        assert_eq!(parsed.sum1(), sample.sum1());
        assert_eq!(parsed.sum2(), sample.sum2());
        assert_eq!(parsed.len(), sample.len());
    }

    #[test]
    fn digest_round_trips_through_le_slice() {
        let sample = RollingDigest::new(0x1357, 0x2468, 1024);
        let parsed = RollingDigest::from_le_slice(&sample.to_le_bytes(), sample.len())
            .expect("slice length matches the digest encoding");

        assert_eq!(parsed, sample);
        assert_eq!(parsed.to_le_bytes(), sample.to_le_bytes());
    }

    #[test]
    fn digest_from_le_slice_rejects_incorrect_length() {
        let error = RollingDigest::from_le_slice(&[0u8; 3], 0)
            .expect_err("three bytes cannot encode a rolling digest");

        assert_eq!(error.len(), 3);
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
    fn checksum_restores_from_digest() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"rolling checksum state");

        let digest = checksum.digest();
        let restored = RollingChecksum::from_digest(digest);

        assert_eq!(restored.digest(), digest);
        assert_eq!(restored.value(), checksum.value());
        assert_eq!(restored.len(), checksum.len());
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

        #[test]
        fn from_digest_round_trips(data in prop::collection::vec(any::<u8>(), 0..=256)) {
            let mut checksum = RollingChecksum::new();
            checksum.update(&data);

            let digest = checksum.digest();
            let restored = RollingChecksum::from_digest(digest);

            prop_assert_eq!(restored.digest(), digest);
            prop_assert_eq!(restored.value(), checksum.value());
            prop_assert_eq!(restored.len(), checksum.len());
        }
    }
}
