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
