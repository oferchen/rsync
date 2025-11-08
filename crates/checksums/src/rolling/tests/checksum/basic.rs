use super::super::*;

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

#[test]
fn simd_availability_matches_architecture_capabilities() {
    let available = simd_acceleration_available();

    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
    {
        assert!(
            available,
            "SIMD acceleration should be reported on architectures with dedicated fast paths"
        );
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        assert!(
            !available,
            "SIMD acceleration must be disabled on unsupported architectures"
        );
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

#[test]
fn update_vectored_coalesces_small_slices() {
    let payload = vec![0xabu8; 1024];
    let mut sequential = RollingChecksum::new();
    for chunk in payload.chunks(7) {
        sequential.update(chunk);
    }

    let slices: Vec<IoSlice<'_>> = payload.chunks(7).map(IoSlice::new).collect();

    let mut vectored = RollingChecksum::new();
    vectored.update_vectored(&slices);

    assert_eq!(vectored.digest(), sequential.digest());
    assert_eq!(vectored.value(), sequential.value());
}
