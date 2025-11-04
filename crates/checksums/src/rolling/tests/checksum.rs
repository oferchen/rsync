use super::super::*;
use super::{chunked_sequences, random_data_and_window, roll_many_sequences};

use proptest::prelude::*;
use std::collections::VecDeque;
use std::io::{self, Cursor, IoSlice, Read};

#[test]
fn checksum_default_digest_is_zero_constant() {
    let checksum = RollingChecksum::new();
    assert_eq!(checksum.digest(), RollingDigest::ZERO);
    assert!(checksum.is_empty());
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
fn roll_many_empty_slices_still_require_initial_window() {
    let mut checksum = RollingChecksum::new();
    let err = checksum
        .roll_many(&[], &[])
        .expect_err("empty slices should still require a seeded window");
    assert_eq!(err, RollingError::EmptyWindow);
    assert_eq!(checksum.digest(), RollingDigest::new(0, 0, 0));
}

#[test]
fn roll_errors_for_window_exceeding_u32() {
    let mut checksum = RollingChecksum::new();
    checksum.force_state(1, 1, (u32::MAX as usize) + 1);

    let err = checksum.roll(0, 0).expect_err("oversized window must fail");
    assert!(matches!(err, RollingError::WindowTooLarge { .. }));
}

#[test]
fn roll_many_errors_for_window_exceeding_u32() {
    let mut checksum = RollingChecksum::new();
    checksum.force_state(1, 1, (u32::MAX as usize) + 1);

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
    let expected = checksum.digest();

    checksum
        .roll_many(&[], &[])
        .expect("empty slices should be ignored");
    assert_eq!(checksum.digest(), expected);
}

#[test]
fn update_reader_matches_manual_update() {
    let data = b"rolling checksum stream input";
    let mut cursor = Cursor::new(&data[..]);

    let mut streamed = RollingChecksum::new();
    let read = streamed
        .update_reader(&mut cursor)
        .expect("reading from cursor succeeds");
    assert_eq!(read, data.len() as u64);

    let mut manual = RollingChecksum::new();
    manual.update(data);

    assert_eq!(streamed.digest(), manual.digest());
    assert_eq!(streamed.value(), manual.value());
}

#[test]
fn update_reader_with_buffer_accepts_small_buffers() {
    let data = b"chunked rolling checksum input";
    let mut cursor = Cursor::new(&data[..]);
    let mut checksum = RollingChecksum::new();
    let mut buffer = [0u8; 3];

    let read = checksum
        .update_reader_with_buffer(&mut cursor, &mut buffer)
        .expect("buffered read succeeds");

    assert_eq!(read, data.len() as u64);

    let mut manual = RollingChecksum::new();
    manual.update(data);

    assert_eq!(checksum.digest(), manual.digest());
    assert_eq!(checksum.value(), manual.value());
}

#[test]
fn saturating_increment_total_clamps_to_u64_max() {
    let mut total = u64::MAX - 1;
    RollingChecksum::saturating_increment_total_for_tests(&mut total, usize::MAX);
    assert_eq!(total, u64::MAX);

    RollingChecksum::saturating_increment_total_for_tests(&mut total, 1);
    assert_eq!(total, u64::MAX);
}

#[test]
fn saturating_increment_total_handles_large_usize_values() {
    let mut total = 0u64;
    RollingChecksum::saturating_increment_total_for_tests(&mut total, usize::MAX);

    if usize::BITS > 64 {
        assert_eq!(total, u64::MAX);
    } else {
        assert_eq!(total, u64::try_from(usize::MAX).unwrap());
    }
}

struct InterruptingReader<'a> {
    inner: Cursor<&'a [u8]>,
    interrupted: bool,
}

impl<'a> InterruptingReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            inner: Cursor::new(data),
            interrupted: false,
        }
    }
}

impl<'a> Read for InterruptingReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            self.inner.read(buf)
        }
    }
}

#[test]
fn update_reader_with_buffer_retries_after_interruption() {
    let data = b"retry after interrupt";
    let mut reader = InterruptingReader::new(data);
    let mut checksum = RollingChecksum::new();
    let mut buffer = [0u8; 8];

    let read = checksum
        .update_reader_with_buffer(&mut reader, &mut buffer)
        .expect("retry succeeds");

    assert_eq!(read, data.len() as u64);
}

#[test]
fn update_reader_with_buffer_rejects_empty_scratch() {
    let data = b"no buffer";
    let mut reader = Cursor::new(&data[..]);
    let mut checksum = RollingChecksum::new();
    let mut buffer = [];

    let err = checksum
        .update_reader_with_buffer(&mut reader, &mut buffer)
        .expect_err("empty buffer must fail");

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn update_reader_with_buffer_retries_after_interruption_preserves_state() {
    let data = b"interrupt once";
    let mut reader = InterruptingReader::new(data);
    let mut checksum = RollingChecksum::new();
    let mut buffer = [0u8; 4];

    checksum
        .update_reader_with_buffer(&mut reader, &mut buffer)
        .expect("retry succeeds");

    let mut manual = RollingChecksum::new();
    manual.update(data);

    assert_eq!(checksum.digest(), manual.digest());
}

#[test]
fn update_vectored_matches_sequential_updates() {
    let chunks = vec![b"hello".to_vec(), b"world".to_vec(), b"!".to_vec()];

    let mut sequential = RollingChecksum::new();
    for chunk in &chunks {
        sequential.update(chunk);
    }

    let slices: Vec<IoSlice<'_>> = chunks
        .iter()
        .map(|chunk| IoSlice::new(chunk.as_slice()))
        .collect();

    let mut vectored = RollingChecksum::new();
    vectored.update_vectored(&slices);

    assert_eq!(vectored.digest(), sequential.digest());
    assert_eq!(vectored.value(), sequential.value());
}

#[test]
fn update_vectored_noop_for_empty_buffer_list() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"seed");
    let digest = checksum.digest();

    checksum.update_vectored(&[]);

    assert_eq!(checksum.digest(), digest);
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
    fn vectored_update_matches_chunked_input(chunks in chunked_sequences()) {
        let mut sequential = RollingChecksum::new();
        for chunk in &chunks {
            sequential.update(chunk);
        }

        let slices: Vec<IoSlice<'_>> =
            chunks.iter().map(|chunk| IoSlice::new(chunk.as_slice())).collect();

        let mut vectored = RollingChecksum::new();
        vectored.update_vectored(&slices);

        prop_assert_eq!(vectored.digest(), sequential.digest());
        prop_assert_eq!(vectored.value(), sequential.value());
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

#[test]
fn roll_many_matches_single_rolls_for_long_sequences() {
    let seed: Vec<u8> = (0..128)
        .map(|value| {
            let byte = value as u8;
            byte.wrapping_mul(13).wrapping_add(5)
        })
        .collect();

    let mut batched = RollingChecksum::new();
    batched.update(&seed);

    let mut reference = batched.clone();

    let mut window = VecDeque::from(seed.clone());
    let mut outgoing = Vec::with_capacity(4096);
    let mut incoming = Vec::with_capacity(4096);

    for step in 0..4096 {
        let leaving = window
            .pop_front()
            .expect("rolling checksum window must contain data");
        let step_byte = step as u8;
        let entering = step_byte
            .wrapping_mul(17)
            .wrapping_add(23)
            .wrapping_add(step_byte >> 3);

        outgoing.push(leaving);
        incoming.push(entering);
        window.push_back(entering);
    }

    batched
        .roll_many(&outgoing, &incoming)
        .expect("batched roll succeeds");

    for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
        reference.roll(out, inn).expect("sequential roll succeeds");
    }

    assert_eq!(batched.digest(), reference.digest());
    assert_eq!(batched.value(), reference.value());
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn sse2_accumulate_matches_scalar_reference() {
    if !std::arch::is_x86_feature_detected!("sse2") {
        return;
    }

    use crate::rolling::checksum::accumulate_chunk_scalar_for_tests;
    use crate::rolling::checksum::x86::accumulate_chunk_sse2_for_tests;

    let sizes = [1usize, 15, 16, 17, 63, 64, 65, 128, 511, 4096];
    let seeds = [
        (0u32, 0u32, 0usize),
        (0x1234u32, 0x5678u32, 7usize),
        (0x0fffu32, 0x7fffu32, 1024usize),
        (0xffffu32, 0xffffu32, usize::MAX - 32),
    ];

    for &(seed_s1, seed_s2, seed_len) in &seeds {
        for &size in &sizes {
            let mut data = vec![0u8; size];
            for (idx, byte) in data.iter_mut().enumerate() {
                *byte = (idx as u8)
                    .wrapping_mul(31)
                    .wrapping_add((size as u8).wrapping_mul(3));
            }

            let scalar = accumulate_chunk_scalar_for_tests(seed_s1, seed_s2, seed_len, &data);
            let simd = accumulate_chunk_sse2_for_tests(seed_s1, seed_s2, seed_len, &data);

            assert_eq!(
                scalar, simd,
                "SSE2 mismatch for size {size} with seeds {seed_s1:#x}/{seed_s2:#x}/{seed_len}",
            );
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn avx2_accumulate_matches_scalar_reference() {
    if !std::arch::is_x86_feature_detected!("avx2") {
        return;
    }

    use crate::rolling::checksum::accumulate_chunk_scalar_for_tests;
    use crate::rolling::checksum::x86::accumulate_chunk_avx2_for_tests;

    let sizes = [32usize, 33, 47, 64, 95, 128, 1024, 4096];
    let seeds = [
        (0u32, 0u32, 0usize),
        (0x1234u32, 0x5678u32, 7usize),
        (0x0fffu32, 0x7fffu32, 1024usize),
        (0xffffu32, 0xffffu32, usize::MAX - 64),
    ];

    for &(seed_s1, seed_s2, seed_len) in &seeds {
        for &size in &sizes {
            let mut data = vec![0u8; size];
            for (idx, byte) in data.iter_mut().enumerate() {
                *byte = (idx as u8)
                    .wrapping_mul(17)
                    .wrapping_add((size as u8).wrapping_mul(5));
            }

            let scalar = accumulate_chunk_scalar_for_tests(seed_s1, seed_s2, seed_len, &data);
            let simd = accumulate_chunk_avx2_for_tests(seed_s1, seed_s2, seed_len, &data);

            assert_eq!(
                scalar, simd,
                "AVX2 mismatch for size {size} with seeds {seed_s1:#x}/{seed_s2:#x}/{seed_len}",
            );
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn neon_accumulate_matches_scalar_reference() {
    use crate::rolling::checksum::accumulate_chunk_scalar_for_tests;
    use crate::rolling::checksum::neon::accumulate_chunk_neon_for_tests;

    let sizes = [1usize, 15, 16, 17, 63, 64, 65, 128, 511, 4096];
    let seeds = [
        (0u32, 0u32, 0usize),
        (0x1234u32, 0x5678u32, 7usize),
        (0x0fffu32, 0x7fffu32, 1024usize),
        (0xffffu32, 0xffffu32, usize::MAX - 32),
    ];

    for &(seed_s1, seed_s2, seed_len) in &seeds {
        for &size in &sizes {
            let mut data = vec![0u8; size];
            for (idx, byte) in data.iter_mut().enumerate() {
                *byte = (idx as u8)
                    .wrapping_mul(29)
                    .wrapping_add((size as u8).wrapping_mul(5));
            }

            let scalar = accumulate_chunk_scalar_for_tests(seed_s1, seed_s2, seed_len, &data);
            let simd = accumulate_chunk_neon_for_tests(seed_s1, seed_s2, seed_len, &data);

            assert_eq!(
                scalar, simd,
                "NEON mismatch for size {size} with seeds {seed_s1:#x}/{seed_s2:#x}/{seed_len}",
            );
        }
    }
}
