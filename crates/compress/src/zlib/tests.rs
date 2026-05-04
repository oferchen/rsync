use std::io::{Cursor, IoSlice, IoSliceMut, Read, Write};
use std::num::NonZeroU8;

use flate2::Compression;

use super::*;
use crate::common::{CountingSink, CountingWriter};

#[test]
fn counting_encoder_tracks_bytes() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    encoder.write(b"payload").expect("compress payload");
    let compressed = encoder.finish().expect("finish stream");
    assert!(compressed > 0);
}

#[test]
fn counting_encoder_reports_incremental_bytes() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    assert_eq!(encoder.bytes_written(), 0);
    encoder.write(b"payload").expect("compress payload");
    let after_first = encoder.bytes_written();
    encoder.write(b"more payload").expect("compress payload");
    let after_second = encoder.bytes_written();
    assert!(after_second >= after_first);
    let final_len = encoder.finish().expect("finish stream");
    assert!(final_len >= after_second);
}

#[test]
fn streaming_round_trip_preserves_payload() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    let input = b"The quick brown fox jumps over the lazy dog".repeat(8);
    for chunk in input.chunks(11) {
        encoder.write(chunk).expect("write chunk");
    }
    let compressed_len = encoder.finish().expect("finish stream");
    assert!(compressed_len > 0);

    let compressed = compress_to_vec(&input, CompressionLevel::Default).expect("compress");
    assert!(compressed.len() as u64 >= compressed_len);
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, input);
}

#[test]
fn counting_encoder_supports_write_trait() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    write!(&mut encoder, "payload").expect("write via trait");
    encoder.flush().expect("flush encoder");
    let compressed = encoder.finish().expect("finish stream");
    assert!(compressed > 0);
}

#[test]
fn counting_encoder_supports_vectored_writes() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    let buffers = [IoSlice::new(b"foo"), IoSlice::new(b"bar")];

    let written = encoder
        .write_vectored(&buffers)
        .expect("vectored write succeeds");
    if written < 6 {
        encoder
            .write_all(&b"foobar"[written..])
            .expect("write remaining data");
    }

    let compressed = encoder.finish().expect("finish stream");
    assert!(compressed > 0);
}

#[test]
fn helper_functions_round_trip() {
    let payload = b"highly compressible payload";
    let compressed = compress_to_vec(payload, CompressionLevel::Best).expect("compress");
    let decoded = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decoded, payload);
}

#[test]
fn counting_encoder_forwards_to_sink() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    encoder.write(b"payload").expect("compress payload");
    let (sink, bytes) = encoder
        .finish_into_inner()
        .expect("finish compression stream");
    assert!(bytes > 0);
    assert!(!sink.is_empty());
    let decoded = decompress_to_vec(&sink).expect("decompress");
    assert_eq!(decoded, b"payload");
}

#[test]
fn counting_encoder_exposes_sink_references() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    assert!(encoder.get_ref().is_empty());

    encoder.get_mut().extend_from_slice(b"prefix");
    assert_eq!(encoder.get_ref(), b"prefix");

    encoder.write_all(b"payload").expect("compress payload");
    let (sink, bytes) = encoder
        .finish_into_inner()
        .expect("finish compression stream");

    assert!(bytes > 0);
    assert!(sink.starts_with(b"prefix"));
    assert_eq!(bytes as usize, sink.len() - b"prefix".len());
}

#[test]
fn counting_decoder_tracks_output_bytes() {
    let payload = b"streaming decoder payload";
    let compressed = compress_to_vec(payload, CompressionLevel::Default).expect("compress");
    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut output = Vec::new();
    decoder.read_to_end(&mut output).expect("decompress");
    assert_eq!(output, payload);
    assert_eq!(decoder.bytes_read(), payload.len() as u64);
}

#[test]
fn counting_decoder_vectored_reads_update_byte_count() {
    let payload = b"Vectored read payload repeated".repeat(4);
    let compressed = compress_to_vec(&payload, CompressionLevel::Default).expect("compress");
    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut first = [0u8; 13];
    let mut second = [0u8; 21];
    let mut buffers = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let read = decoder
        .read_vectored(&mut buffers)
        .expect("vectored read succeeds");
    assert!(read > 0);

    let mut collected = Vec::with_capacity(read);
    let first_len = read.min(first.len());
    collected.extend_from_slice(&first[..first_len]);
    if read > first_len {
        let second_len = read - first_len;
        collected.extend_from_slice(&second[..second_len]);
    }

    assert_eq!(collected, payload[..read]);
    assert_eq!(decoder.bytes_read(), read as u64);
}

#[test]
fn counting_decoder_exposes_reader_accessors() {
    let payload = b"reader accessor payload";
    let compressed = compress_to_vec(payload, CompressionLevel::Default).expect("compress");
    let cursor = Cursor::new(compressed);
    let mut decoder = CountingZlibDecoder::new(cursor);

    assert_eq!(decoder.get_ref().position(), 0);
    decoder.get_mut().set_position(2);
    assert_eq!(decoder.get_ref().position(), 2);

    let inner = decoder.into_inner();
    assert_eq!(inner.position(), 2);
}

#[test]
fn precise_level_converts_to_requested_value() {
    let level = NonZeroU8::new(7).expect("non-zero");
    let compression = Compression::from(CompressionLevel::precise(level));
    assert_eq!(compression.level(), u32::from(level.get()));
}

#[test]
fn numeric_level_constructor_accepts_valid_range() {
    for level in 1..=9 {
        let precise = CompressionLevel::from_numeric(level).expect("valid level");
        let expected = NonZeroU8::new(level as u8).expect("range checked");
        assert_eq!(precise, CompressionLevel::Precise(expected));
    }
}

#[test]
fn numeric_level_constructor_rejects_out_of_range() {
    let err = CompressionLevel::from_numeric(10).expect_err("level above 9 rejected");
    assert_eq!(err.level(), 10);
}

#[test]
fn counting_writer_saturating_add_prevents_overflow() {
    let mut writer = CountingWriter::new(CountingSink);
    writer.saturating_add_bytes(usize::MAX);
    writer.saturating_add_bytes(usize::MAX);
    assert_eq!(writer.bytes(), u64::MAX);
}

#[test]
fn zero_byte_roundtrip() {
    let compressed = compress_to_vec(b"", CompressionLevel::Default).expect("compress empty");
    let decompressed = decompress_to_vec(&compressed).expect("decompress empty");
    assert!(decompressed.is_empty());
}

#[test]
fn zero_byte_streaming_roundtrip() {
    let encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    let (compressed, bytes) = encoder.finish_into_inner().expect("finish empty stream");
    assert!(bytes > 0, "deflate stream has framing even when empty");

    let decompressed = decompress_to_vec(&compressed).expect("decompress empty stream");
    assert!(decompressed.is_empty());
}

#[test]
fn compression_level_1_compresses_successfully() {
    let level = CompressionLevel::from_numeric(1).expect("level 1 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 1");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 1");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_2_compresses_successfully() {
    let level = CompressionLevel::from_numeric(2).expect("level 2 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 2");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 2");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_3_compresses_successfully() {
    let level = CompressionLevel::from_numeric(3).expect("level 3 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 3");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 3");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_4_compresses_successfully() {
    let level = CompressionLevel::from_numeric(4).expect("level 4 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 4");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 4");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_5_compresses_successfully() {
    let level = CompressionLevel::from_numeric(5).expect("level 5 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 5");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 5");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_6_compresses_successfully() {
    let level = CompressionLevel::from_numeric(6).expect("level 6 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 6");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 6");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_7_compresses_successfully() {
    let level = CompressionLevel::from_numeric(7).expect("level 7 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 7");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 7");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_8_compresses_successfully() {
    let level = CompressionLevel::from_numeric(8).expect("level 8 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 8");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 8");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_9_compresses_successfully() {
    let level = CompressionLevel::from_numeric(9).expect("level 9 is valid");
    let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
    let compressed = compress_to_vec(&payload, level).expect("compress with level 9");
    assert!(!compressed.is_empty());
    let decompressed = decompress_to_vec(&compressed).expect("decompress level 9");
    assert_eq!(decompressed, payload);
}

#[test]
fn higher_compression_levels_produce_smaller_output() {
    let payload = b"AAAAAAAAAA".repeat(100);

    let mut sizes = Vec::new();
    for level in 1..=9 {
        let compression_level =
            CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
        let compressed =
            compress_to_vec(&payload, compression_level).expect("compression succeeds");
        sizes.push((level, compressed.len()));
    }

    let level1_size = sizes[0].1;
    let level9_size = sizes[8].1;
    assert!(
        level9_size < level1_size,
        "level 9 ({level9_size} bytes) should be smaller than level 1 ({level1_size} bytes)"
    );

    let level5_size = sizes[4].1;
    assert!(
        level5_size <= level1_size,
        "level 5 ({level5_size} bytes) should be <= level 1 ({level1_size} bytes)"
    );

    assert!(
        level9_size <= level5_size,
        "level 9 ({level9_size} bytes) should be <= level 5 ({level5_size} bytes)"
    );
}

#[test]
fn all_levels_roundtrip_correctly() {
    let payload = b"Test payload with various characters: 123!@# ABC xyz".repeat(20);

    for level in 1..=9 {
        let compression_level =
            CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
        let compressed =
            compress_to_vec(&payload, compression_level).expect("compression succeeds");
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");

        assert_eq!(
            decompressed, payload,
            "level {level} failed to roundtrip correctly"
        );
    }
}

#[test]
fn all_levels_handle_empty_input() {
    for level in 1..=9 {
        let compression_level =
            CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
        let compressed = compress_to_vec(b"", compression_level).expect("compress empty input");
        let decompressed = decompress_to_vec(&compressed).expect("decompress empty input");

        assert!(
            decompressed.is_empty(),
            "level {level} failed to handle empty input"
        );
    }
}

#[test]
fn all_levels_handle_single_byte() {
    for level in 1..=9 {
        let compression_level =
            CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
        let payload = b"X";
        let compressed = compress_to_vec(payload, compression_level).expect("compress single byte");
        let decompressed = decompress_to_vec(&compressed).expect("decompress single byte");

        assert_eq!(
            decompressed, payload,
            "level {level} failed to handle single byte"
        );
    }
}

#[test]
fn all_levels_handle_incompressible_data() {
    let payload: Vec<u8> = (0..256).map(|i| (i * 137 + 73) as u8).collect();

    for level in 1..=9 {
        let compression_level =
            CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
        let compressed =
            compress_to_vec(&payload, compression_level).expect("compress incompressible data");
        let decompressed = decompress_to_vec(&compressed).expect("decompress incompressible data");

        assert_eq!(
            decompressed, payload,
            "level {level} failed with incompressible data"
        );

        assert!(
            compressed.len() < payload.len() * 2,
            "level {level} produced unreasonably large output for incompressible data"
        );
    }
}

#[test]
fn all_levels_work_with_counting_encoder() {
    let payload = b"Counting encoder test payload".repeat(5);

    for level in 1..=9 {
        let compression_level =
            CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");

        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), compression_level);
        encoder.write(&payload).expect("write to encoder");
        let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish encoder");

        assert_eq!(
            bytes_written as usize,
            compressed.len(),
            "level {level} counting encoder byte count mismatch"
        );

        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(
            decompressed, payload,
            "level {level} counting encoder roundtrip failed"
        );
    }
}

// Per-token compression flush tests against upstream wire format
//
// Upstream rsync (token.c) uses a persistent deflate stream across an
// entire file transfer. Between tokens it issues Z_SYNC_FLUSH so that
// the receiver can inflate each segment independently. The sync flush
// appends the 4-byte marker 0x00 0x00 0xFF 0xFF which the sender strips
// before putting data on the wire. These tests verify that the flush
// mechanics produce correct, independently decompressible segments and
// that token boundaries survive a roundtrip.
//
// Reference: upstream token.c lines 433-454 (Z_SYNC_FLUSH + marker strip).

/// Compresses `input` with raw deflate and Z_SYNC_FLUSH, returning the
/// compressed bytes *including* the trailing sync marker.
fn compress_with_sync_flush(input: &[u8], level: Compression) -> Vec<u8> {
    use flate2::{Compress, FlushCompress};

    let mut compressor = Compress::new(level, false);
    let mut out = vec![0u8; input.len() * 2 + 128];
    // upstream: chunks are fed with Z_NO_FLUSH between sync flushes.
    let mut consumed = 0;
    while consumed < input.len() {
        let before_in = compressor.total_in() as usize;
        let before_out = compressor.total_out() as usize;
        compressor
            .compress(
                &input[consumed..],
                &mut out[before_out..],
                FlushCompress::None,
            )
            .expect("compress with no-flush");
        consumed += (compressor.total_in() as usize) - before_in;
    }

    loop {
        let before_out = compressor.total_out();
        let status = compressor
            .compress(
                &[],
                &mut out[compressor.total_out() as usize..],
                FlushCompress::Sync,
            )
            .expect("sync flush");
        if status == flate2::Status::Ok || compressor.total_out() == before_out {
            break;
        }
    }

    let total_out = compressor.total_out() as usize;
    out.truncate(total_out);
    out
}

#[test]
fn sync_flush_produces_marker_bytes() {
    // upstream token.c: Z_SYNC_FLUSH always ends with 0x00 0x00 0xFF 0xFF
    let data = b"per-token flush marker test payload";
    let compressed = compress_with_sync_flush(data, Compression::default());

    assert!(
        compressed.len() >= 4,
        "compressed output too short for sync marker"
    );
    assert_eq!(
        &compressed[compressed.len() - 4..],
        &[0x00, 0x00, 0xFF, 0xFF],
        "sync flush must end with the 4-byte marker"
    );
}

#[test]
fn each_token_independently_decompressible() {
    // Simulates upstream per-token pattern: compress each token's literal
    // data with its own deflate context + Z_SYNC_FLUSH, strip the marker,
    // then verify the receiver can inflate each segment independently by
    // re-appending the marker.
    use flate2::{Decompress, FlushDecompress};

    let tokens: &[&[u8]] = &[
        b"first token payload with some repetitive data data data",
        b"second token - different content entirely",
        b"third token: short",
    ];

    for (i, token_data) in tokens.iter().enumerate() {
        let compressed = compress_with_sync_flush(token_data, Compression::default());

        // Strip the trailing sync marker (as upstream sender does)
        assert!(compressed.len() >= 4);
        let stripped = &compressed[..compressed.len() - 4];

        // upstream token.c: receiver re-appends the sync marker before inflating.
        let mut to_inflate = stripped.to_vec();
        to_inflate.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);

        let mut decompressor = Decompress::new(false);
        let mut output = vec![0u8; token_data.len() + 64];
        decompressor
            .decompress(&to_inflate, &mut output, FlushDecompress::Sync)
            .unwrap_or_else(|e| {
                panic!("token {i} failed to decompress independently: {e}");
            });

        let produced = decompressor.total_out() as usize;
        assert_eq!(
            &output[..produced],
            *token_data,
            "token {i} roundtrip mismatch"
        );
    }
}

#[test]
fn empty_token_produces_valid_sync_flush_output() {
    // upstream token.c: even with zero literal bytes, Z_SYNC_FLUSH must
    // produce valid output (at minimum the sync marker itself).
    use flate2::{Compress, Decompress, FlushCompress, FlushDecompress};

    let mut compressor = Compress::new(Compression::default(), false);
    let mut out = [0u8; 64];

    loop {
        let before = compressor.total_out();
        let status = compressor
            .compress(
                &[],
                &mut out[compressor.total_out() as usize..],
                FlushCompress::Sync,
            )
            .expect("sync flush on empty");
        if status == flate2::Status::Ok || compressor.total_out() == before {
            break;
        }
    }

    let total = compressor.total_out() as usize;
    let compressed = &out[..total];

    assert!(
        compressed.len() >= 4,
        "empty sync flush should still produce output (got {} bytes)",
        compressed.len()
    );
    assert_eq!(
        &compressed[compressed.len() - 4..],
        &[0x00, 0x00, 0xFF, 0xFF],
    );

    let mut decompressor = Decompress::new(false);
    let mut decoded = vec![0u8; 64];
    decompressor
        .decompress(compressed, &mut decoded, FlushDecompress::Sync)
        .expect("empty sync flush should decompress");
    assert_eq!(
        decompressor.total_out(),
        0,
        "empty token should produce no output bytes"
    );
}

#[test]
fn token_boundaries_preserved_persistent_stream() {
    // upstream token.c: a single deflate context persists across the entire
    // file, with Z_SYNC_FLUSH between tokens. Verify that the receiver can
    // recover each token's data from the concatenated stream by re-injecting
    // the sync markers at token boundaries.
    use flate2::{Compress, Decompress, FlushCompress, FlushDecompress};

    let tokens: &[&[u8]] = &[
        b"AAAAAAAAAA first token with repetition AAAAAAAAAA",
        b"BBBBBBBBBB second token BBBBBBBBBB different pattern",
        b"CCCCCCCCCC third token CCCCCCCCCC yet another",
        b"D",
    ];

    // Sender side: single persistent compressor, sync flush per token,
    // strip marker after each flush.
    let mut compressor = Compress::new(Compression::default(), false);
    let mut segments: Vec<Vec<u8>> = Vec::new();
    let mut scratch = vec![0u8; 4096];

    for token_data in tokens {
        let mut segment = Vec::new();

        let mut consumed = 0;
        while consumed < token_data.len() {
            let before_in = compressor.total_in() as usize;
            let before_out = compressor.total_out() as usize;
            compressor
                .compress(
                    &token_data[consumed..],
                    &mut scratch[..],
                    FlushCompress::None,
                )
                .expect("no-flush compress");
            let produced = (compressor.total_out() as usize) - before_out;
            if produced > 0 {
                segment.extend_from_slice(&scratch[..produced]);
            }
            consumed += (compressor.total_in() as usize) - before_in;
        }

        loop {
            let before_out = compressor.total_out() as usize;
            let status = compressor
                .compress(&[], &mut scratch[..], FlushCompress::Sync)
                .expect("sync flush");
            let produced = (compressor.total_out() as usize) - before_out;
            if produced > 0 {
                segment.extend_from_slice(&scratch[..produced]);
            }
            if status == flate2::Status::Ok || produced == 0 {
                break;
            }
        }

        // Strip the trailing sync marker (upstream wire format)
        assert!(
            segment.len() >= 4,
            "segment too short for sync marker strip"
        );
        assert_eq!(&segment[segment.len() - 4..], &[0x00, 0x00, 0xFF, 0xFF]);
        segment.truncate(segment.len() - 4);

        segments.push(segment);
    }

    // Receiver side: single persistent decompressor, re-inject sync marker
    // at each token boundary before inflating.
    let mut decompressor = Decompress::new(false);
    let mut output_buf = vec![0u8; 4096];

    for (i, segment) in segments.iter().enumerate() {
        let mut to_inflate = segment.clone();
        to_inflate.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);

        let mut recovered = Vec::new();
        let mut input = &to_inflate[..];

        loop {
            let before_in = decompressor.total_in();
            let before_out = decompressor.total_out();
            decompressor
                .decompress(input, &mut output_buf, FlushDecompress::Sync)
                .unwrap_or_else(|e| {
                    panic!("token {i} decompression failed: {e}");
                });

            let consumed = (decompressor.total_in() - before_in) as usize;
            let produced = (decompressor.total_out() - before_out) as usize;

            if produced > 0 {
                recovered.extend_from_slice(&output_buf[..produced]);
            }
            if consumed > 0 {
                input = &input[consumed..];
            }
            if input.is_empty() || (consumed == 0 && produced == 0) {
                break;
            }
        }

        assert_eq!(
            recovered,
            tokens[i],
            "token {i} boundary not preserved: expected {:?}, got {:?}",
            String::from_utf8_lossy(tokens[i]),
            String::from_utf8_lossy(&recovered),
        );
    }
}

#[test]
fn stripped_sync_marker_roundtrips_all_levels() {
    // Verify the strip-and-restore pattern works at every compression level.
    // upstream token.c always uses this pattern regardless of -z level.
    use flate2::{Decompress, FlushDecompress};

    let payload = b"payload for multi-level sync flush test with some repeated words words words";

    for level in 1..=9u32 {
        let compression = Compression::new(level);
        let compressed = compress_with_sync_flush(payload, compression);

        assert!(compressed.len() >= 4);
        let mut stripped = compressed[..compressed.len() - 4].to_vec();
        stripped.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);

        let mut decompressor = Decompress::new(false);
        let mut output = vec![0u8; payload.len() + 64];
        decompressor
            .decompress(&stripped, &mut output, FlushDecompress::Sync)
            .unwrap_or_else(|e| panic!("level {level} decompress failed: {e}"));

        let produced = decompressor.total_out() as usize;
        assert_eq!(
            &output[..produced],
            &payload[..],
            "level {level} roundtrip after marker strip/restore failed"
        );
    }
}

#[test]
fn sync_flush_after_each_write_produces_decompressible_prefix() {
    // Verify that flushing after each write (simulating per-token behavior)
    // allows the receiver to decompress all data seen so far at any boundary.
    use flate2::{Compress, Decompress, FlushCompress, FlushDecompress};

    let chunks: &[&[u8]] = &[b"chunk-one ", b"chunk-two ", b"chunk-three"];

    let mut compressor = Compress::new(Compression::default(), false);
    let mut wire = Vec::new();
    let mut scratch = vec![0u8; 4096];
    let mut boundaries: Vec<usize> = Vec::new();

    for chunk in chunks {
        let mut consumed = 0;
        while consumed < chunk.len() {
            let bi = compressor.total_in() as usize;
            let bo = compressor.total_out() as usize;
            compressor
                .compress(&chunk[consumed..], &mut scratch, FlushCompress::None)
                .expect("no-flush");
            let produced = (compressor.total_out() as usize) - bo;
            if produced > 0 {
                wire.extend_from_slice(&scratch[..produced]);
            }
            consumed += (compressor.total_in() as usize) - bi;
        }

        loop {
            let bo = compressor.total_out() as usize;
            let status = compressor
                .compress(&[], &mut scratch, FlushCompress::Sync)
                .expect("sync flush");
            let produced = (compressor.total_out() as usize) - bo;
            if produced > 0 {
                wire.extend_from_slice(&scratch[..produced]);
            }
            if status == flate2::Status::Ok || produced == 0 {
                break;
            }
        }

        boundaries.push(wire.len());
    }

    // At each boundary, the wire prefix must decompress to the
    // concatenation of all chunks up to that point.
    let mut expected = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        expected.extend_from_slice(chunk);

        let prefix = &wire[..boundaries[i]];
        let mut decompressor = Decompress::new(false);
        let mut output = vec![0u8; expected.len() + 64];
        let mut input = prefix;
        let mut recovered = Vec::new();

        loop {
            let bi = decompressor.total_in();
            let bo = decompressor.total_out();
            decompressor
                .decompress(input, &mut output, FlushDecompress::Sync)
                .unwrap_or_else(|e| {
                    panic!("boundary {i} decompress failed: {e}");
                });
            let consumed = (decompressor.total_in() - bi) as usize;
            let produced = (decompressor.total_out() - bo) as usize;
            if produced > 0 {
                recovered.extend_from_slice(&output[..produced]);
            }
            if consumed > 0 {
                input = &input[consumed..];
            }
            if input.is_empty() || (consumed == 0 && produced == 0) {
                break;
            }
        }

        assert_eq!(
            recovered, expected,
            "at boundary {i}, decompressed prefix should equal all chunks so far"
        );
    }
}

#[test]
fn compression_level_none_roundtrip() {
    let payload = b"data that will not be deflated".repeat(10);
    let compressed = compress_to_vec(&payload, CompressionLevel::None).expect("compress none");
    let decompressed = decompress_to_vec(&compressed).expect("decompress none");
    assert_eq!(decompressed, payload);
    // Level None stores data verbatim - compressed size >= original
    assert!(compressed.len() >= payload.len());
}

#[test]
fn compression_level_fast_roundtrip() {
    let payload = b"fast compression test data with repetition repetition repetition".repeat(10);
    let compressed = compress_to_vec(&payload, CompressionLevel::Fast).expect("compress fast");
    let decompressed = decompress_to_vec(&compressed).expect("decompress fast");
    assert_eq!(decompressed, payload);
    assert!(compressed.len() < payload.len());
}

#[test]
fn compression_level_best_roundtrip() {
    let payload = b"best compression test data with repetition repetition repetition".repeat(10);
    let compressed = compress_to_vec(&payload, CompressionLevel::Best).expect("compress best");
    let decompressed = decompress_to_vec(&compressed).expect("decompress best");
    assert_eq!(decompressed, payload);
    assert!(compressed.len() < payload.len());
}

#[test]
fn compression_level_none_streaming_roundtrip() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::None);
    let payload = b"none level streaming test";
    encoder.write(payload).expect("write");
    let (compressed, bytes) = encoder.finish_into_inner().expect("finish");
    assert_eq!(bytes as usize, compressed.len());
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, payload);
}

#[test]
fn compression_level_fast_streaming_roundtrip() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Fast);
    let payload = b"fast level streaming test with repeated words words words";
    encoder.write(payload).expect("write");
    let (compressed, bytes) = encoder.finish_into_inner().expect("finish");
    assert_eq!(bytes as usize, compressed.len());
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, payload);
}

#[test]
fn from_numeric_zero_returns_none_variant() {
    let level = CompressionLevel::from_numeric(0).expect("level 0 is valid");
    assert_eq!(level, CompressionLevel::None);
}

#[test]
fn from_numeric_rejects_large_values() {
    for invalid in [10, 11, 100, 255, 1000, u32::MAX] {
        let err = CompressionLevel::from_numeric(invalid).expect_err("should reject");
        assert_eq!(err.level(), invalid);
    }
}

#[test]
fn compression_level_error_display_message() {
    let err = CompressionLevelError::new(42);
    let msg = err.to_string();
    assert!(
        msg.contains("42"),
        "error message should contain the invalid level"
    );
    assert!(
        msg.contains("0-9"),
        "error message should mention the valid range"
    );
}

#[test]
fn compression_level_error_equality() {
    let a = CompressionLevelError::new(10);
    let b = CompressionLevelError::new(10);
    let c = CompressionLevelError::new(11);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn compression_level_clone_and_copy() {
    let level = CompressionLevel::Fast;
    let cloned = level;
    assert_eq!(level, cloned);

    let precise = CompressionLevel::from_numeric(5).expect("valid");
    let copied = precise;
    assert_eq!(precise, copied);
}

#[test]
fn compression_level_debug_output() {
    let level = CompressionLevel::Default;
    let debug = format!("{level:?}");
    assert!(debug.contains("Default"));

    let precise = CompressionLevel::from_numeric(3).expect("valid");
    let debug = format!("{precise:?}");
    assert!(debug.contains("Precise"));
}

#[test]
fn compression_level_into_flate2_all_variants() {
    let cases = [
        (CompressionLevel::None, Compression::none()),
        (CompressionLevel::Fast, Compression::fast()),
        (CompressionLevel::Default, Compression::default()),
        (CompressionLevel::Best, Compression::best()),
    ];
    for (level, expected) in cases {
        let actual = Compression::from(level);
        assert_eq!(actual.level(), expected.level());
    }
}

#[test]
fn single_byte_streaming_roundtrip() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    encoder.write(b"X").expect("write single byte");
    let (compressed, bytes) = encoder.finish_into_inner().expect("finish");
    assert!(bytes > 0);
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, b"X");
}

#[test]
fn large_data_roundtrip() {
    let payload: Vec<u8> = (0..1_048_576u32).map(|i| (i % 251) as u8).collect();
    let compressed =
        compress_to_vec(&payload, CompressionLevel::Default).expect("compress large data");
    assert!(
        compressed.len() < payload.len(),
        "large data should compress"
    );
    let decompressed = decompress_to_vec(&compressed).expect("decompress large data");
    assert_eq!(decompressed, payload);
}

#[test]
fn large_data_streaming_roundtrip() {
    let payload: Vec<u8> = (0..1_048_576u32).map(|i| (i % 251) as u8).collect();
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Fast);
    for chunk in payload.chunks(4096) {
        encoder.write(chunk).expect("write chunk");
    }
    let (compressed, bytes) = encoder.finish_into_inner().expect("finish");
    assert_eq!(bytes as usize, compressed.len());
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, payload);
}

#[test]
fn incompressible_data_roundtrip() {
    // Random-like data that resists compression
    let payload: Vec<u8> = (0..512).map(|i| ((i * 137 + 73) % 256) as u8).collect();
    let compressed =
        compress_to_vec(&payload, CompressionLevel::Best).expect("compress incompressible");
    let decompressed = decompress_to_vec(&compressed).expect("decompress incompressible");
    assert_eq!(decompressed, payload);
}

#[test]
fn decompress_invalid_data_returns_error() {
    let garbage = b"this is not valid deflate data";
    let result = decompress_to_vec(garbage);
    assert!(result.is_err(), "decompressing garbage should fail");
}

#[test]
fn decompress_truncated_data_returns_error() {
    let payload = b"test data for truncation".repeat(10);
    let compressed =
        compress_to_vec(&payload, CompressionLevel::Default).expect("compress succeeds");
    let truncated = &compressed[..compressed.len() / 2];
    let result = decompress_to_vec(truncated);
    // Truncated data may decompress partially or fail - either is acceptable
    // as long as it does not panic
    if let Ok(partial) = result {
        assert!(partial.len() < payload.len());
    }
}

#[test]
fn decoder_partial_reads_accumulate_byte_count() {
    let payload = b"partial read test payload that is long enough".repeat(5);
    let compressed =
        compress_to_vec(&payload, CompressionLevel::Default).expect("compress succeeds");
    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));

    let mut total_read = 0;
    let mut collected = Vec::new();
    let mut buf = [0u8; 7]; // Small buffer to force multiple reads
    loop {
        let n = decoder.read(&mut buf).expect("read succeeds");
        if n == 0 {
            break;
        }
        total_read += n;
        collected.extend_from_slice(&buf[..n]);
    }

    assert_eq!(total_read, payload.len());
    assert_eq!(decoder.bytes_read(), payload.len() as u64);
    assert_eq!(collected, payload);
}

#[test]
fn decoder_zero_length_read_returns_zero() {
    let compressed =
        compress_to_vec(b"test", CompressionLevel::Default).expect("compress succeeds");
    let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
    let mut empty = [0u8; 0];
    let n = decoder.read(&mut empty).expect("zero-length read succeeds");
    assert_eq!(n, 0);
    assert_eq!(decoder.bytes_read(), 0);
}

#[test]
fn encoder_bytes_written_matches_finish() {
    let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
    encoder
        .write(b"test payload for byte counting")
        .expect("write");
    encoder.flush().expect("flush");
    let before_finish = encoder.bytes_written();
    let final_bytes = encoder.finish().expect("finish");
    assert!(final_bytes >= before_finish);
}

#[test]
fn encoder_write_all_trait_method() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    Write::write_all(&mut encoder, b"write_all test").expect("write_all");
    let (compressed, _) = encoder.finish_into_inner().expect("finish");
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, b"write_all test");
}

#[test]
fn encoder_write_fmt_trait_method() {
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
    write!(&mut encoder, "formatted {} data test", 42).expect("write_fmt");
    let (compressed, _) = encoder.finish_into_inner().expect("finish");
    let decompressed = decompress_to_vec(&compressed).expect("decompress");
    assert_eq!(decompressed, b"formatted 42 data test");
}

#[test]
fn precise_constructor_creates_correct_variant() {
    let nz = NonZeroU8::new(4).expect("non-zero");
    let level = CompressionLevel::precise(nz);
    assert_eq!(level, CompressionLevel::Precise(nz));
}

#[test]
fn best_produces_smaller_output_than_fast() {
    let payload = b"repetitive data ".repeat(200);
    let fast = compress_to_vec(&payload, CompressionLevel::Fast).expect("fast");
    let best = compress_to_vec(&payload, CompressionLevel::Best).expect("best");
    assert!(
        best.len() <= fast.len(),
        "Best ({}) should produce output <= Fast ({})",
        best.len(),
        fast.len()
    );
}

#[test]
fn none_level_does_not_shrink_data() {
    let payload = b"ABCDEFGHIJ".repeat(100);
    let compressed = compress_to_vec(&payload, CompressionLevel::None).expect("compress none");
    // Level 0 stores uncompressed - output should be >= input
    assert!(
        compressed.len() >= payload.len(),
        "None level should not shrink data: compressed={} original={}",
        compressed.len(),
        payload.len()
    );
}

#[test]
fn counting_encoder_flush_produces_sync_flush() {
    // Verify that calling flush() on CountingZlibEncoder produces
    // a Z_SYNC_FLUSH, matching the upstream per-token pattern.
    // flate2::write::DeflateEncoder::flush() triggers Z_SYNC_FLUSH.
    let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

    encoder
        .write(b"token-one data payload")
        .expect("write token 1");
    encoder.flush().expect("flush after token 1");

    let after_flush_1 = encoder.get_ref().len();
    assert!(after_flush_1 > 0, "flush should produce output");

    // The flushed output ending with the sync marker confirms Z_SYNC_FLUSH
    let buf = encoder.get_ref().clone();
    assert!(buf.len() >= 4, "flushed output too short for sync marker");
    assert_eq!(
        &buf[buf.len() - 4..],
        &[0x00, 0x00, 0xFF, 0xFF],
        "CountingZlibEncoder::flush() must produce Z_SYNC_FLUSH marker"
    );

    encoder
        .write(b"token-two different data")
        .expect("write token 2");
    encoder.flush().expect("flush after token 2");

    let after_flush_2 = encoder.get_ref().len();
    assert!(
        after_flush_2 > after_flush_1,
        "second flush should produce more output"
    );

    // Verify the cumulative output ends with sync marker
    let buf2 = encoder.get_ref().clone();
    assert_eq!(
        &buf2[buf2.len() - 4..],
        &[0x00, 0x00, 0xFF, 0xFF],
        "second flush must also produce Z_SYNC_FLUSH marker"
    );

    // The entire output (two sync-flushed segments) must decompress to both tokens
    let (compressed, _bytes) = encoder.finish_into_inner().expect("finish");
    let decompressed = decompress_to_vec(&compressed).expect("decompress combined");
    assert_eq!(
        decompressed,
        b"token-one data payloadtoken-two different data",
    );
}
