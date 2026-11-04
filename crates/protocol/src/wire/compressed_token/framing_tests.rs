//! Wire framing of a literal run against a direct port of upstream's sender.
//!
//! `token_c_literal_run` transcribes `token.c:send_deflated_token()`
//! (lines 416-474) for a file sent as one literal run, which is how upstream
//! sends a file with no basis: `match.c` calls `matched(.., -1)` once with the
//! whole file, so the deflate loop sees every byte in one call. It drives the
//! same `flate2` backend as the encoder, so it isolates the framing - chunking,
//! flush points, piece boundaries, the 4-byte sync-trailer strip - from the
//! deflate library, which is a documented deviation (`docs/UPSTREAM_COMPARISON.md`
//! section 12).

use std::num::NonZeroU8;

use compress::zlib::CompressionLevel;
use flate2::{Compress, Compression, FlushCompress};

use super::{CHUNK_SIZE, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT};

/// Upstream's sender for one literal run, then the end-of-file flag.
///
/// `obuf` mirrors upstream's output buffer: two header bytes, then up to
/// `MAX_DATA_COUNT` payload bytes. A piece is written only when the buffer
/// fills or the input is exhausted, and once the final chunk is fed with
/// `Z_SYNC_FLUSH` a full buffer keeps its last 4 bytes for the next piece in
/// case they are the sync trailer (token.c:434-443, 454-461).
fn token_c_literal_run(data: &[u8], level: u32) -> Vec<u8> {
    let mut wire = Vec::new();
    if !data.is_empty() {
        let mut strm = Compress::new(Compression::new(level), false);
        let mut obuf = vec![0u8; MAX_DATA_COUNT + 2];
        let mut flush = FlushCompress::None;
        let mut input: &[u8] = &[];
        let (mut nb, mut offset) = (data.len(), 0);
        let (mut out_pos, mut avail_out) = (2, 0);
        loop {
            if input.is_empty() && nb != 0 {
                let n = nb.min(CHUNK_SIZE);
                input = &data[offset..offset + n];
                nb -= n;
                offset += n;
            }
            if avail_out == 0 {
                out_pos = 2;
                avail_out = MAX_DATA_COUNT;
                if flush != FlushCompress::None {
                    obuf.copy_within(MAX_DATA_COUNT - 2..MAX_DATA_COUNT + 2, 2);
                    out_pos += 4;
                    avail_out -= 4;
                }
            }
            if nb == 0 {
                flush = FlushCompress::Sync;
            }
            let (before_in, before_out) = (strm.total_in(), strm.total_out());
            strm.compress(input, &mut obuf[out_pos..out_pos + avail_out], flush)
                .expect("deflate");
            input = &input[(strm.total_in() - before_in) as usize..];
            let produced = (strm.total_out() - before_out) as usize;
            out_pos += produced;
            avail_out -= produced;
            if nb == 0 || avail_out == 0 {
                let mut n = MAX_DATA_COUNT - avail_out;
                if flush != FlushCompress::None {
                    n -= 4;
                }
                if n > 0 {
                    wire.push(DEFLATED_DATA + (n >> 8) as u8);
                    wire.push(n as u8);
                    wire.extend_from_slice(&obuf[2..2 + n]);
                }
            }
            if nb == 0 && avail_out != 0 {
                break;
            }
        }
    }
    wire.push(END_FLAG);
    wire
}

/// The encoder's wire output for `data` as one literal run, fed in uneven
/// slices so chunking cannot depend on the caller's write sizes.
fn encoder_literal_run(data: &[u8], level: u32, zlibx: bool) -> Vec<u8> {
    let level = CompressionLevel::precise(NonZeroU8::new(level as u8).expect("level"));
    let mut encoder = CompressedTokenEncoder::new(level, 31);
    encoder.set_zlibx(zlibx);
    let mut wire = Vec::new();
    for slice in data.chunks(7919) {
        encoder
            .send_literal(&mut wire, slice)
            .expect("send literal");
    }
    encoder.finish(&mut wire).expect("finish");
    wire
}

/// The DEFLATED_DATA payload lengths of a literal-only stream, for a readable
/// failure message.
fn piece_lengths(wire: &[u8]) -> Vec<usize> {
    let mut lengths = Vec::new();
    let mut at = 0;
    while at + 1 < wire.len() && wire[at] & 0xC0 == DEFLATED_DATA {
        let len = (usize::from(wire[at] & 0x3F) << 8) | usize::from(wire[at + 1]);
        lengths.push(len);
        at += 2 + len;
    }
    lengths
}

/// A paragraph repeated to 200 KiB: long-distance repeats, tiny output.
fn repeated_paragraph() -> Vec<u8> {
    const PARA: &[u8] = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n";
    PARA.iter().copied().cycle().take(200 * 1024).collect()
}

/// Numbered lines: locally repetitive text, output well past one piece.
fn numbered_lines() -> Vec<u8> {
    (0..4000)
        .flat_map(|i| {
            format!("line {i:06}: the quick brown fox jumps over the lazy dog\n").into_bytes()
        })
        .take(200 * 1024)
        .collect()
}

/// Deterministic incompressible bytes, so the final sync flush spans several
/// full pieces and the flush-phase 4-byte carry is exercised.
fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

#[test]
fn literal_run_framing_matches_token_c() {
    let fixtures: [(&str, Vec<u8>); 5] = [
        ("repeated paragraph", repeated_paragraph()),
        ("numbered lines", numbered_lines()),
        ("incompressible", incompressible(100 * 1024)),
        ("exact chunk multiple", incompressible(2 * CHUNK_SIZE)),
        ("one byte", vec![b'x']),
    ];
    for (name, data) in &fixtures {
        for level in [1, 6, 9] {
            let expected = token_c_literal_run(data, level);
            for zlibx in [false, true] {
                let actual = encoder_literal_run(data, level, zlibx);
                assert!(
                    actual == expected,
                    "{name}, level {level}, zlibx {zlibx}: framing differs from token.c; pieces {:?} vs {:?}",
                    piece_lengths(&actual),
                    piece_lengths(&expected)
                );
            }
        }
    }
}

/// Wire sizes with the zlib-ng / zlib-rs deflate this crate ships with.
///
/// The framing is pinned by `literal_run_framing_matches_token_c`; this pins
/// the library's output so a codec regression that keeps the framing valid but
/// inflates the stream is caught. Upstream 3.5.1 links classic zlib, which
/// gives smaller streams on the repeated paragraph; both are listed in
/// `docs/UPSTREAM_COMPARISON.md` section 12.
#[cfg(any(feature = "zlib-ng", feature = "zlib-rs"))]
#[test]
fn literal_run_wire_size_is_pinned_for_the_shipped_deflate() {
    let cases = [
        (
            "repeated paragraph",
            repeated_paragraph(),
            [2575usize, 1543, 813],
        ),
        ("numbered lines", numbered_lines(), [15586, 9403, 9113]),
    ];
    for (name, data, expected) in &cases {
        let sizes = [1, 6, 9].map(|level| encoder_literal_run(data, level, false).len());
        assert_eq!(&sizes, expected, "{name}: wire bytes at levels 1/6/9");
    }
}
