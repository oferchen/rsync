use super::*;
use std::io::Cursor;

// ==== Construction and Basic Methods ====

#[test]
fn rolling_checksum_new_creates_empty_state() {
    let checksum = RollingChecksum::new();
    assert!(checksum.is_empty());
    assert_eq!(checksum.len(), 0);
    assert_eq!(checksum.value(), 0);
}

#[test]
fn rolling_checksum_default_equals_new() {
    let new = RollingChecksum::new();
    let default = RollingChecksum::default();
    assert_eq!(new, default);
}

#[test]
fn rolling_checksum_reset_clears_state() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"some data");
    assert!(!checksum.is_empty());

    checksum.reset();
    assert!(checksum.is_empty());
    assert_eq!(checksum.len(), 0);
    assert_eq!(checksum.value(), 0);
}

#[test]
fn rolling_checksum_from_digest_reconstructs_state() {
    let mut original = RollingChecksum::new();
    original.update(b"test data");
    let digest = original.digest();

    let reconstructed = RollingChecksum::from_digest(digest);
    assert_eq!(original.value(), reconstructed.value());
    assert_eq!(original.len(), reconstructed.len());
}

#[test]
fn rolling_checksum_clone_equals_original() {
    let mut original = RollingChecksum::new();
    original.update(b"clone test");
    let cloned = original.clone();
    assert_eq!(original, cloned);
}

#[test]
fn rolling_checksum_debug_format_contains_name() {
    let checksum = RollingChecksum::new();
    let debug = format!("{checksum:?}");
    assert!(debug.contains("RollingChecksum"));
}

#[test]
fn rolling_checksum_equality() {
    let mut a = RollingChecksum::new();
    let mut b = RollingChecksum::new();
    assert_eq!(a, b);

    a.update(b"same");
    b.update(b"same");
    assert_eq!(a, b);

    let mut c = RollingChecksum::new();
    c.update(b"different");
    assert_ne!(a, c);
}

// ==== update() method ====

#[test]
fn update_empty_slice_is_noop() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"");
    assert!(checksum.is_empty());
    assert_eq!(checksum.value(), 0);
}

#[test]
fn update_single_byte() {
    let mut checksum = RollingChecksum::new();
    checksum.update(&[0x42]);
    assert_eq!(checksum.len(), 1);
    assert!(!checksum.is_empty());
}

#[test]
fn update_multiple_times_accumulates() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"Hello, ");
    checksum.update(b"World!");
    let partial_value = checksum.value();

    let mut full = RollingChecksum::new();
    full.update(b"Hello, World!");
    assert_eq!(checksum.value(), full.value());
    assert_eq!(checksum.len(), full.len());
    assert_eq!(partial_value, full.value());
}

#[test]
fn update_small_chunk() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"abc");
    assert_eq!(checksum.len(), 3);
    assert_ne!(checksum.value(), 0);
}

#[test]
fn update_exact_four_bytes() {
    // Tests the chunks_exact(4) path
    let mut checksum = RollingChecksum::new();
    checksum.update(b"1234");
    assert_eq!(checksum.len(), 4);
}

#[test]
fn update_sixteen_bytes() {
    // Multiple chunks of 4
    let mut checksum = RollingChecksum::new();
    checksum.update(b"0123456789ABCDEF");
    assert_eq!(checksum.len(), 16);
}

#[test]
fn update_with_remainder() {
    // 6 bytes = 1 chunk of 4 + 2 remainder
    let mut checksum = RollingChecksum::new();
    checksum.update(b"123456");
    assert_eq!(checksum.len(), 6);
}

#[test]
fn update_large_chunk() {
    let data = vec![0x55u8; 4096];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    assert_eq!(checksum.len(), 4096);
}

#[test]
fn update_from_block_resets_and_updates() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"initial");

    checksum.update_from_block(b"fresh");
    assert_eq!(checksum.len(), 5);

    let mut fresh = RollingChecksum::new();
    fresh.update(b"fresh");
    assert_eq!(checksum.value(), fresh.value());
}

// ==== update_vectored() method ====

#[test]
fn update_vectored_empty_slices() {
    let mut checksum = RollingChecksum::new();
    let slices: &[IoSlice<'_>] = &[];
    checksum.update_vectored(slices);
    assert!(checksum.is_empty());
}

#[test]
fn update_vectored_single_slice() {
    let mut checksum = RollingChecksum::new();
    let data = b"single buffer";
    let slices = [IoSlice::new(data)];
    checksum.update_vectored(&slices);
    assert_eq!(checksum.len(), data.len());

    let mut direct = RollingChecksum::new();
    direct.update(data);
    assert_eq!(checksum.value(), direct.value());
}

#[test]
fn update_vectored_multiple_slices() {
    let mut checksum = RollingChecksum::new();
    let data1 = b"Hello, ";
    let data2 = b"World!";
    let slices = [IoSlice::new(data1), IoSlice::new(data2)];
    checksum.update_vectored(&slices);
    assert_eq!(checksum.len(), data1.len() + data2.len());

    let mut direct = RollingChecksum::new();
    direct.update(b"Hello, World!");
    assert_eq!(checksum.value(), direct.value());
}

#[test]
fn update_vectored_with_empty_slices() {
    let mut checksum = RollingChecksum::new();
    let data = b"data";
    let empty: &[u8] = b"";
    let slices = [IoSlice::new(empty), IoSlice::new(data), IoSlice::new(empty)];
    checksum.update_vectored(&slices);
    assert_eq!(checksum.len(), data.len());
}

#[test]
fn update_vectored_large_slice_flushes() {
    // A slice >= VECTORED_STACK_CAPACITY (128) should flush
    let mut checksum = RollingChecksum::new();
    let large_data = vec![0xAAu8; 256];
    let slices = [IoSlice::new(&large_data)];
    checksum.update_vectored(&slices);
    assert_eq!(checksum.len(), 256);
}

#[test]
fn update_vectored_fills_scratch_exactly() {
    // VECTORED_STACK_CAPACITY = 128
    let mut checksum = RollingChecksum::new();
    let data = vec![0xBBu8; 128];
    let slices = [IoSlice::new(&data)];
    checksum.update_vectored(&slices);
    assert_eq!(checksum.len(), 128);
}

#[test]
fn update_vectored_overflow_scratch() {
    // Two slices that together exceed scratch capacity
    let mut checksum = RollingChecksum::new();
    let data1 = vec![0xCCu8; 100];
    let data2 = vec![0xDDu8; 50];
    let slices = [IoSlice::new(&data1), IoSlice::new(&data2)];
    checksum.update_vectored(&slices);
    assert_eq!(checksum.len(), 150);
}

// ==== update_reader() and update_reader_with_buffer() ====

#[test]
fn empty_reader_buffer_is_rejected() {
    let mut c = RollingChecksum::new();
    let mut rdr = &b""[..];
    let mut buf: [u8; 0] = [];
    let err = c.update_reader_with_buffer(&mut rdr, &mut buf).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn update_reader_with_buffer_empty_reader() {
    let mut checksum = RollingChecksum::new();
    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut buffer = [0u8; 64];
    let total = checksum
        .update_reader_with_buffer(&mut reader, &mut buffer)
        .unwrap();
    assert_eq!(total, 0);
    assert!(checksum.is_empty());
}

#[test]
fn update_reader_with_buffer_small_data() {
    let mut checksum = RollingChecksum::new();
    let data = b"small data";
    let mut reader = Cursor::new(data.to_vec());
    let mut buffer = [0u8; 4];
    let total = checksum
        .update_reader_with_buffer(&mut reader, &mut buffer)
        .unwrap();
    assert_eq!(total, data.len() as u64);
    assert_eq!(checksum.len(), data.len());

    let mut direct = RollingChecksum::new();
    direct.update(data);
    assert_eq!(checksum.value(), direct.value());
}

#[test]
fn update_reader_with_buffer_larger_than_buffer() {
    let mut checksum = RollingChecksum::new();
    let data = vec![0x77u8; 1000];
    let mut reader = Cursor::new(data);
    let mut buffer = [0u8; 64]; // Buffer smaller than data
    let total = checksum
        .update_reader_with_buffer(&mut reader, &mut buffer)
        .unwrap();
    assert_eq!(total, 1000);
    assert_eq!(checksum.len(), 1000);
}

#[test]
fn update_reader_default_buffer() {
    let mut checksum = RollingChecksum::new();
    let data = b"test with default buffer";
    let mut reader = Cursor::new(data.to_vec());
    let total = checksum.update_reader(&mut reader).unwrap();
    assert_eq!(total, data.len() as u64);
    assert_eq!(checksum.len(), data.len());
}

#[test]
fn update_reader_accumulates_with_existing_state() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"prefix-");

    let data = b"suffix";
    let mut reader = Cursor::new(data.to_vec());
    checksum.update_reader(&mut reader).unwrap();

    let mut full = RollingChecksum::new();
    full.update(b"prefix-suffix");
    assert_eq!(checksum.value(), full.value());
}

// ==== roll() method ====

#[test]
fn roll_on_empty_window_fails() {
    let mut checksum = RollingChecksum::new();
    let result = checksum.roll(b'a', b'b');
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), RollingError::EmptyWindow));
}

#[test]
fn roll_single_position() {
    let data = b"ABCDE";
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[0..3]); // "ABC"
    rolling.roll(data[0], data[3]).unwrap(); // Remove A, add D -> "BCD"

    let mut fresh = RollingChecksum::new();
    fresh.update(&data[1..4]); // "BCD"
    assert_eq!(rolling.value(), fresh.value());
}

#[test]
fn roll_preserves_window_length() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"ABCD");
    let len_before = checksum.len();
    checksum.roll(b'A', b'E').unwrap();
    assert_eq!(checksum.len(), len_before);
}

#[test]
fn roll_multiple_times() {
    let data = b"ABCDEFGH";
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[0..4]); // "ABCD"

    // Roll through: ABCD -> BCDE -> CDEF -> DEFG
    rolling.roll(b'A', b'E').unwrap();
    rolling.roll(b'B', b'F').unwrap();
    rolling.roll(b'C', b'G').unwrap();

    let mut fresh = RollingChecksum::new();
    fresh.update(&data[3..7]); // "DEFG"
    assert_eq!(rolling.value(), fresh.value());
}

#[test]
fn roll_same_byte() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"AAAA");
    let value_before = checksum.value();
    checksum.roll(b'A', b'A').unwrap(); // Same byte in and out
    assert_eq!(checksum.value(), value_before);
}

// ==== roll_many() method ====

#[test]
fn roll_many_on_empty_window_fails() {
    let mut checksum = RollingChecksum::new();
    let result = checksum.roll_many(&[1, 2], &[3, 4]);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), RollingError::EmptyWindow));
}

#[test]
fn roll_many_mismatched_lengths_fails() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"ABCD");
    let result = checksum.roll_many(&[1, 2], &[3]);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RollingError::MismatchedSliceLength { .. }
    ));
}

#[test]
fn roll_many_empty_slices_is_noop() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"ABCD");
    let value_before = checksum.value();
    checksum.roll_many(&[], &[]).unwrap();
    assert_eq!(checksum.value(), value_before);
}

#[test]
fn roll_many_single_byte() {
    let data = b"ABCDE";
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[0..4]); // "ABCD"
    rolling.roll_many(&data[0..1], &data[4..5]).unwrap(); // A -> E

    let mut fresh = RollingChecksum::new();
    fresh.update(&data[1..5]); // "BCDE"
    assert_eq!(rolling.value(), fresh.value());
}

#[test]
fn roll_many_multiple_bytes() {
    let data = b"ABCDEFGH";
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[0..4]); // "ABCD"

    // Roll by 3 positions: ABC out, EFG in
    // Result should be window shifted by 3
    rolling.roll_many(&data[0..3], &data[4..7]).unwrap();
    // One more to complete: D out, H in
    rolling.roll(data[3], data[7]).unwrap();

    let mut fresh = RollingChecksum::new();
    fresh.update(&data[4..8]); // "EFGH"
    assert_eq!(rolling.value(), fresh.value());
}

#[test]
fn roll_many_equals_repeated_roll() {
    let data = b"0123456789ABCDEF";
    let mut rolling_many = RollingChecksum::new();
    rolling_many.update(&data[0..8]);
    rolling_many.roll_many(&data[0..4], &data[8..12]).unwrap();

    let mut rolling_single = RollingChecksum::new();
    rolling_single.update(&data[0..8]);
    for i in 0..4 {
        rolling_single.roll(data[i], data[8 + i]).unwrap();
    }

    assert_eq!(rolling_many.value(), rolling_single.value());
}

// ==== value() and digest() methods ====

#[test]
fn value_format_is_s2_high_s1_low() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"test");
    let value = checksum.value();
    let s1 = value & 0xFFFF;
    let s2 = (value >> 16) & 0xFFFF;

    // Verify components match digest
    let digest = checksum.digest();
    assert_eq!(s1 as u16, digest.sum1());
    assert_eq!(s2 as u16, digest.sum2());
}

#[test]
fn digest_roundtrip() {
    let mut original = RollingChecksum::new();
    original.update(b"digest test data");
    let digest = original.digest();

    let reconstructed = RollingChecksum::from_digest(digest);
    assert_eq!(original.value(), reconstructed.value());
    assert_eq!(original.len(), reconstructed.len());
}

// ==== From trait implementations ====

#[test]
fn from_rolling_digest_for_rolling_checksum() {
    let digest = RollingDigest::new(0x1234, 0x5678, 100);
    let checksum: RollingChecksum = digest.into();
    assert_eq!(checksum.len(), 100);
}

#[test]
fn from_rolling_checksum_for_rolling_digest() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"convert");
    let digest: RollingDigest = checksum.clone().into();
    assert_eq!(digest.len(), checksum.len());
}

#[test]
fn from_rolling_checksum_ref_for_rolling_digest() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"ref convert");
    let digest: RollingDigest = (&checksum).into();
    assert_eq!(digest.len(), checksum.len());
}

// ==== simd_acceleration_available() ====

#[test]
fn simd_acceleration_available_returns_bool() {
    // Just verify it doesn't panic and returns a boolean
    let _available = simd_acceleration_available();
}

// ==== Scalar fallback tests ====

#[test]
fn scalar_accumulate_empty_chunk() {
    let (s1, s2, len) = accumulate_chunk_scalar_for_tests(0, 0, 0, &[]);
    assert_eq!(s1, 0);
    assert_eq!(s2, 0);
    assert_eq!(len, 0);
}

#[test]
fn scalar_accumulate_single_byte() {
    let (s1, s2, len) = accumulate_chunk_scalar_for_tests(0, 0, 0, &[42]);
    assert_eq!(s1, 42);
    assert_eq!(s2, 42);
    assert_eq!(len, 1);
}

#[test]
fn scalar_accumulate_four_bytes() {
    let data = [1u8, 2, 3, 4];
    let (s1, s2, len) = accumulate_chunk_scalar_for_tests(0, 0, 0, &data);
    // s1 = 1+2+3+4 = 10
    // s2 = 1 + (1+2) + (1+2+3) + (1+2+3+4) = 1 + 3 + 6 + 10 = 20
    assert_eq!(s1, 10);
    assert_eq!(s2, 20);
    assert_eq!(len, 4);
}

#[test]
fn scalar_accumulate_with_initial_state() {
    let (s1, s2, len) = accumulate_chunk_scalar_for_tests(100, 200, 50, &[10, 20]);
    // s1 = 100 + 10 + 20 = 130
    // s2 = 200 + 110 + 130 = 440
    assert_eq!(s1, 130);
    assert_eq!(s2, 440);
    assert_eq!(len, 52);
}

#[test]
fn scalar_accumulate_with_remainder() {
    // 5 bytes = 1 chunk of 4 + 1 remainder
    let data = [1u8, 1, 1, 1, 1];
    let (s1, _s2, len) = accumulate_chunk_scalar_for_tests(0, 0, 0, &data);
    assert_eq!(s1, 5);
    assert_eq!(len, 5);
}

// ==== Saturating increment tests ====

#[test]
fn saturating_increment_total_normal() {
    let mut total = 100u64;
    RollingChecksum::saturating_increment_total_for_tests(&mut total, 50);
    assert_eq!(total, 150);
}

#[test]
fn saturating_increment_total_at_max() {
    let mut total = u64::MAX;
    RollingChecksum::saturating_increment_total_for_tests(&mut total, 100);
    assert_eq!(total, u64::MAX);
}

// ==== Edge cases ====

#[test]
fn deterministic_checksum() {
    let data = b"deterministic test";
    let mut c1 = RollingChecksum::new();
    c1.update(data);

    let mut c2 = RollingChecksum::new();
    c2.update(data);

    assert_eq!(c1.value(), c2.value());
}

#[test]
fn different_data_different_checksum() {
    let mut c1 = RollingChecksum::new();
    c1.update(b"data one");

    let mut c2 = RollingChecksum::new();
    c2.update(b"data two");

    assert_ne!(c1.value(), c2.value());
}

#[test]
fn checksum_order_matters() {
    let mut c1 = RollingChecksum::new();
    c1.update(b"AB");

    let mut c2 = RollingChecksum::new();
    c2.update(b"BA");

    assert_ne!(c1.value(), c2.value());
}

#[test]
fn force_state_sets_internal_values() {
    let mut checksum = RollingChecksum::new();
    checksum.force_state(0x1234, 0x5678, 100);
    assert_eq!(checksum.len(), 100);
    // value = (s2 << 16) | s1 = (0x5678 << 16) | 0x1234
    let expected_value = (0x5678u32 << 16) | 0x1234u32;
    assert_eq!(checksum.value(), expected_value);
}

#[test]
fn default_reader_buffer_len_constant() {
    // Verify constant is reasonable for efficient I/O
    let len = RollingChecksum::DEFAULT_READER_BUFFER_LEN;
    assert_ne!(len, 0);
    assert!(len >= 1024, "buffer should be at least 1KB, got {len}");
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn x86_cpu_feature_detection_is_cached() {
    x86::load_cpu_features_for_tests();
    assert!(x86::cpu_features_cached_for_tests());
}

// ==== Inter-architecture compatibility golden tests ====
//
// These tests verify the checksum algorithm produces identical results across all
// supported architectures (x86 SSE2/AVX2, ARM NEON, scalar fallback). The expected
// values are computed once and hardcoded, ensuring any SIMD optimization changes
// don't break compatibility.

#[test]
fn golden_test_empty_input() {
    let mut checksum = RollingChecksum::new();
    checksum.update(b"");
    assert_eq!(checksum.value(), 0x0000_0000);
    assert_eq!(checksum.len(), 0);
}

#[test]
fn golden_test_single_byte() {
    let mut checksum = RollingChecksum::new();
    checksum.update(&[0x42]); // 'B' = 66
    // s1 = 66, s2 = 66, value = (66 << 16) | 66 = 0x0042_0042
    assert_eq!(checksum.value(), 0x0042_0042);
    assert_eq!(checksum.digest().sum1(), 66);
    assert_eq!(checksum.digest().sum2(), 66);
}

#[test]
fn golden_test_known_string() {
    // "rsync" = [0x72, 0x73, 0x79, 0x6e, 0x63]
    let mut checksum = RollingChecksum::new();
    checksum.update(b"rsync");
    // s1 = 0x72 + 0x73 + 0x79 + 0x6e + 0x63 = 114 + 115 + 121 + 110 + 99 = 559 = 0x022f
    // s2 = 114 + 229 + 350 + 460 + 559 = 1712 = 0x06b0
    // value = (s2 << 16) | s1 = 0x06b0_022f
    assert_eq!(checksum.value(), 0x06b0_022f);
}

#[test]
fn golden_test_all_zeros() {
    // 16 zero bytes
    let data = [0u8; 16];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    // All zeros: s1 = 0, s2 = 0
    assert_eq!(checksum.value(), 0x0000_0000);
}

#[test]
fn golden_test_all_ones() {
    // 16 bytes of 0x01
    let data = [1u8; 16];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    // s1 = 16 * 1 = 16 = 0x0010
    // s2 = 1 + 2 + 3 + ... + 16 = 16*17/2 = 136 = 0x0088
    // value = (136 << 16) | 16 = 0x0088_0010
    assert_eq!(checksum.value(), 0x0088_0010);
}

#[test]
fn golden_test_all_ff() {
    // 16 bytes of 0xFF
    let data = [0xFFu8; 16];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    // s1 = 16 * 255 = 4080 = 0x0ff0
    // s2 = 255*1 + 255*2 + ... + 255*16 = 255 * (16*17/2) = 255 * 136 = 34680 = 0x8778
    // value = (s2 << 16) | s1 = 0x8778_0ff0
    assert_eq!(checksum.value(), 0x8778_0ff0);
}

#[test]
fn golden_test_ascending_bytes() {
    // [0, 1, 2, ..., 15]
    let data: Vec<u8> = (0..16).collect();
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    // s1 = 0 + 1 + 2 + ... + 15 = 15*16/2 = 120 = 0x0078
    // s2 = 0*1 + 1*2 + 2*3 + ... - weighted by position
    // Using the formula: sum_{i=0}^{n-1} i*(n-i) = n*(n-1)*(n+1)/6
    // For n=16: s2 = sum of prefix sums
    // Prefix sums: 0, 1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 66, 78, 91, 105, 120
    // s2 = 680 = 0x02a8
    // value = (680 << 16) | 120 = 0x02a8_0078
    assert_eq!(checksum.value(), 0x02a8_0078);
}

#[test]
fn golden_test_block_length_700() {
    // rsync commonly uses 700-byte blocks for small files
    // Create deterministic data pattern
    let mut data = vec![0u8; 700];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = ((i * 31) % 256) as u8;
    }
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    // Pre-computed golden value (architecture-independent)
    // This value was verified against the scalar implementation
    assert_eq!(checksum.value(), 0xe2ea_5c96);
    assert_eq!(checksum.len(), 700);
}

#[test]
fn golden_test_block_length_4096() {
    // 4KB block size
    let mut data = vec![0u8; 4096];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = ((i * 17 + 7) % 256) as u8;
    }
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);
    // Pre-computed golden value (architecture-independent)
    assert_eq!(checksum.value(), 0x2000_f800);
    assert_eq!(checksum.len(), 4096);
}

#[test]
fn golden_test_incremental_matches_full() {
    // Verify incremental updates produce same result as full update
    let data = b"The quick brown fox jumps over the lazy dog";

    let mut full = RollingChecksum::new();
    full.update(data);

    let mut incremental = RollingChecksum::new();
    incremental.update(&data[..10]);
    incremental.update(&data[10..25]);
    incremental.update(&data[25..]);

    assert_eq!(full.value(), incremental.value());
    assert_eq!(full.value(), 0x5ba2_0fd9);
}

#[test]
fn golden_test_roll_produces_expected_value() {
    // Verify rolling produces correct checksum after shift
    let data = b"ABCDEFGH";
    let mut checksum = RollingChecksum::new();
    checksum.update(&data[0..4]); // "ABCD"

    // Verify initial value
    assert_eq!(checksum.value(), 0x0294_010a);

    // Roll: remove 'A', add 'E'
    checksum.roll(b'A', b'E').unwrap();

    // Should match fresh computation of "BCDE"
    let mut fresh = RollingChecksum::new();
    fresh.update(&data[1..5]);
    assert_eq!(checksum.value(), fresh.value());
    assert_eq!(checksum.value(), 0x029e_010e);
}
