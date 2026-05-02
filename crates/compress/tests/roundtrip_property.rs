//! Property-based round-trip tests for the compress codecs.
//!
//! Goal: for every supported compression level of every codec, an arbitrary
//! input must round-trip through `compress -> decompress` to a byte-identical
//! buffer. The test matrix sweeps:
//!
//! - codec: zlib (always) and zstd (when `cfg(feature = "zstd")`),
//! - level: every preset and every numeric level in the codec's supported range,
//! - size: 0, 1, small, several boundaries inside `0..=64 KiB`,
//! - shape: all-zeros, all-`0xFF`, repeating pattern (highly compressible),
//!   ramp/permutation (moderately compressible), and a deterministic LCG-driven
//!   pseudo-random buffer (incompressible).
//!
//! In addition, streaming flush boundaries are exercised: callers write the
//! input in slices, optionally calling `flush` between slices, and confirm
//! that the decoder reassembles the original bytes regardless of the chunk
//! and flush layout. This mirrors the upstream contract documented in
//! `token.c:send_token()` (encoder writes incrementally with periodic
//! flushes) and `token.c:simple_recv_token()` (decoder reassembles tokens
//! incrementally on the receiving side).

use std::io::Read;
use std::num::NonZeroU8;

use compress::zlib::{self, CompressionLevel, CountingZlibDecoder, CountingZlibEncoder};

#[cfg(feature = "zstd")]
use compress::zstd::{self, CountingZstdDecoder, CountingZstdEncoder};

/// Sizes (in bytes) that exercise interesting boundaries inside the
/// `0..=64 KiB` window: zero, one byte, sub-block, deflate block edges, and
/// 64 KiB exactly.
const SIZES: &[usize] = &[
    0, 1, 2, 15, 16, 17, 255, 256, 257, 1023, 1024, 1025, 4095, 4096, 4097, 8191, 8192, 8193,
    16_383, 16_384, 16_385, 32_767, 32_768, 32_769, 65_535, 65_536,
];

/// Deterministic LCG. Avoids pulling in `rand` so tests stay reproducible and
/// self-contained.
fn lcg_bytes(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let mut buf = Vec::with_capacity(size);
    for _ in 0..size {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        buf.push((state >> 56) as u8);
    }
    buf
}

/// Distinct payload shapes covering compressible, incompressible, and pathological inputs.
fn shaped_inputs(size: usize) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    out.push(("zeros", vec![0u8; size]));
    out.push(("ones", vec![0xFFu8; size]));

    // Repeating pattern. Highly compressible.
    let pattern = b"oc-rsync property roundtrip|";
    let mut repeat = Vec::with_capacity(size);
    while repeat.len() < size {
        let take = pattern.len().min(size - repeat.len());
        repeat.extend_from_slice(&pattern[..take]);
    }
    out.push(("repeat", repeat));

    // Ramp containing every byte value, moderately compressible.
    let ramp: Vec<u8> = (0..size).map(|i| (i & 0xFF) as u8).collect();
    out.push(("ramp", ramp));

    // Pseudo-random, near-incompressible.
    out.push(("lcg", lcg_bytes(size, 0xC0FFEE_u64 ^ size as u64)));

    out
}

/// Slice layouts that exercise streaming write boundaries. Each entry is a
/// list of slice lengths whose sum must equal `total`.
fn streaming_layouts(total: usize) -> Vec<Vec<usize>> {
    if total == 0 {
        return vec![vec![]];
    }
    let mut layouts: Vec<Vec<usize>> = Vec::new();
    layouts.push(vec![total]);
    if total >= 2 {
        layouts.push(vec![1, total - 1]);
        let half = total / 2;
        layouts.push(vec![half, total - half]);
    }
    if total >= 4 {
        let q = total / 4;
        layouts.push(vec![q, q, q, total - 3 * q]);
    }
    if total >= 8 {
        // Many small slices to stress per-write framing.
        let mut chunked = Vec::new();
        let chunk = (total / 8).max(1);
        let mut remaining = total;
        while remaining > chunk {
            chunked.push(chunk);
            remaining -= chunk;
        }
        chunked.push(remaining);
        layouts.push(chunked);
    }
    layouts
}

/// Iterator over every supported zlib level: `None`, `Fast`, `Default`, `Best`,
/// plus every numeric level `0..=9`.
fn zlib_levels() -> Vec<(String, CompressionLevel)> {
    let mut levels = vec![
        ("None".to_string(), CompressionLevel::None),
        ("Fast".to_string(), CompressionLevel::Fast),
        ("Default".to_string(), CompressionLevel::Default),
        ("Best".to_string(), CompressionLevel::Best),
    ];
    for n in 0u32..=9 {
        levels.push((
            format!("Numeric({n})"),
            CompressionLevel::from_numeric(n).expect("0..=9 is valid for zlib"),
        ));
    }
    levels
}

/// Iterator over every supported zstd level: presets plus the full
/// `1..=22` numeric range. Level 0 maps to `None`.
#[cfg(feature = "zstd")]
fn zstd_levels() -> Vec<(String, CompressionLevel)> {
    let mut levels = vec![
        ("None".to_string(), CompressionLevel::None),
        ("Fast".to_string(), CompressionLevel::Fast),
        ("Default".to_string(), CompressionLevel::Default),
        ("Best".to_string(), CompressionLevel::Best),
    ];
    for n in 1u8..=22 {
        levels.push((
            format!("Numeric({n})"),
            CompressionLevel::Precise(NonZeroU8::new(n).expect("1..=22")),
        ));
    }
    levels
}

#[test]
fn zlib_round_trip_every_level_every_shape() {
    for (level_name, level) in zlib_levels() {
        for &size in SIZES {
            for (shape_name, input) in shaped_inputs(size) {
                let compressed = zlib::compress_to_vec(&input, level).unwrap_or_else(|err| {
                    panic!(
                        "zlib compress failed: level={level_name} size={size} shape={shape_name} err={err}"
                    )
                });
                let decompressed = zlib::decompress_to_vec(&compressed).unwrap_or_else(|err| {
                    panic!(
                        "zlib decompress failed: level={level_name} size={size} shape={shape_name} err={err}"
                    )
                });
                assert_eq!(
                    decompressed, input,
                    "zlib round-trip mismatch: level={level_name} size={size} shape={shape_name}"
                );
            }
        }
    }
}

#[test]
fn zlib_streaming_flush_boundaries_round_trip() {
    // upstream: token.c:send_token() emits compressed bytes incrementally,
    // and token.c:simple_recv_token() reassembles them on the receiving end.
    // The implementation must therefore tolerate arbitrary write/flush layouts.
    for (level_name, level) in zlib_levels() {
        // Restrict the streaming sweep to a representative set of sizes to keep
        // the matrix tractable while still spanning sub-block / multi-block.
        for &size in &[0usize, 1, 17, 256, 1024, 4096, 16_384, 65_536] {
            for (shape_name, input) in shaped_inputs(size) {
                for layout in streaming_layouts(size) {
                    for flush_between in [false, true] {
                        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), level);
                        let mut offset = 0usize;
                        for &chunk_len in &layout {
                            encoder.write(&input[offset..offset + chunk_len]).unwrap();
                            offset += chunk_len;
                            if flush_between {
                                std::io::Write::flush(&mut encoder).unwrap();
                            }
                        }
                        assert_eq!(offset, input.len());
                        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
                        assert_eq!(
                            bytes as usize,
                            compressed.len(),
                            "zlib bytes_written disagrees with sink length: \
                             level={level_name} size={size} shape={shape_name}",
                        );

                        let mut decoder = CountingZlibDecoder::new(&compressed[..]);
                        let mut decompressed = Vec::new();
                        decoder.read_to_end(&mut decompressed).unwrap();
                        assert_eq!(
                            decoder.bytes_read(),
                            input.len() as u64,
                            "zlib decoder bytes_read mismatch: \
                             level={level_name} size={size} shape={shape_name}",
                        );
                        assert_eq!(
                            decompressed, input,
                            "zlib streaming round-trip mismatch: \
                             level={level_name} size={size} shape={shape_name} \
                             layout={layout:?} flush={flush_between}",
                        );
                    }
                }
            }
        }
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_round_trip_every_level_every_shape() {
    for (level_name, level) in zstd_levels() {
        for &size in SIZES {
            for (shape_name, input) in shaped_inputs(size) {
                let compressed = zstd::compress_to_vec(&input, level).unwrap_or_else(|err| {
                    panic!(
                        "zstd compress failed: level={level_name} size={size} shape={shape_name} err={err}"
                    )
                });
                let decompressed = zstd::decompress_to_vec(&compressed).unwrap_or_else(|err| {
                    panic!(
                        "zstd decompress failed: level={level_name} size={size} shape={shape_name} err={err}"
                    )
                });
                assert_eq!(
                    decompressed, input,
                    "zstd round-trip mismatch: level={level_name} size={size} shape={shape_name}"
                );
            }
        }
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_streaming_flush_boundaries_round_trip() {
    // upstream: token.c:send_token() / token.c:simple_recv_token() - the
    // streaming contract requires the decoder to recover the original bytes
    // regardless of how the encoder partitioned its writes or where flushes
    // landed in the stream.
    for (level_name, level) in zstd_levels() {
        for &size in &[0usize, 1, 17, 256, 1024, 4096, 16_384, 65_536] {
            for (shape_name, input) in shaped_inputs(size) {
                for layout in streaming_layouts(size) {
                    for flush_between in [false, true] {
                        let mut encoder =
                            CountingZstdEncoder::with_sink(Vec::new(), level).unwrap();
                        let mut offset = 0usize;
                        for &chunk_len in &layout {
                            encoder.write(&input[offset..offset + chunk_len]).unwrap();
                            offset += chunk_len;
                            if flush_between {
                                encoder.flush().unwrap();
                            }
                        }
                        assert_eq!(offset, input.len());
                        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
                        assert_eq!(
                            bytes as usize,
                            compressed.len(),
                            "zstd bytes_written disagrees with sink length: \
                             level={level_name} size={size} shape={shape_name}",
                        );

                        let mut decoder = CountingZstdDecoder::new(&compressed[..]).unwrap();
                        let mut decompressed = Vec::new();
                        decoder.read_to_end(&mut decompressed).unwrap();
                        assert_eq!(
                            decoder.bytes_read(),
                            input.len() as u64,
                            "zstd decoder bytes_read mismatch: \
                             level={level_name} size={size} shape={shape_name}",
                        );
                        assert_eq!(
                            decompressed, input,
                            "zstd streaming round-trip mismatch: \
                             level={level_name} size={size} shape={shape_name} \
                             layout={layout:?} flush={flush_between}",
                        );
                    }
                }
            }
        }
    }
}
