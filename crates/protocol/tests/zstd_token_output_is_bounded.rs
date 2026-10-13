#![cfg(feature = "zstd")]
//! A zstd DEFLATED_DATA packet is inflated in bounded, upstream-sized steps.
//!
//! The DEFLATED_DATA length field caps one packet at `MAX_DATA_COUNT` (16383)
//! compressed bytes, but zstd turns that many bytes of RLE blocks into
//! hundreds of MiB. Upstream never materialises a packet: each
//! `ZSTD_decompressStream` step writes into a fixed
//! `out_buffer_size = ZSTD_DStreamOutSize() * 2` buffer and that step's output
//! is returned as one literal before the next step runs, so receiver memory
//! does not depend on the compression ratio. A decoder that inflates the whole
//! packet before emitting anything lets a hostile sender force an allocation of
//! about 0.5 GB per 16 KiB on the wire.
//!
//! upstream: token.c:854 `out_buffer_size = ZSTD_DStreamOutSize() * 2`;
//! token.c:895-919 recv_zstd_token() r_inflating - one step per call, the
//! input position carried in `zstd_in_buff` across calls.

use std::io::Cursor;

use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT,
};

/// Decompressed size of the hostile packet: far above one decoder step.
const INFLATED_LEN: usize = 64 * 1024 * 1024;

/// upstream: token.c:854 - the per-step output buffer of recv_zstd_token().
fn upstream_out_buffer_size() -> usize {
    zstd::zstd_safe::DCtx::out_size() * 2
}

/// A zstd frame of `INFLATED_LEN` zero bytes, built without holding the
/// inflated data in memory.
fn zero_frame() -> Vec<u8> {
    use std::io::Write;
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..INFLATED_LEN / chunk.len() {
        encoder.write_all(&chunk).unwrap();
    }
    encoder.finish().unwrap()
}

/// Frames `payload` as one DEFLATED_DATA packet followed by END_FLAG.
fn single_packet(payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= MAX_DATA_COUNT,
        "fixture must fit one DEFLATED_DATA packet, got {} bytes",
        payload.len()
    );
    let mut wire = vec![
        DEFLATED_DATA | (payload.len() >> 8) as u8,
        (payload.len() & 0xFF) as u8,
    ];
    wire.extend_from_slice(payload);
    wire.push(END_FLAG);
    wire
}

#[test]
fn zstd_packet_inflates_in_upstream_sized_steps_and_round_trips() {
    let wire = single_packet(&zero_frame());
    let step_limit = upstream_out_buffer_size();
    let mut decoder = CompressedTokenDecoder::new_zstd().unwrap();
    let mut cursor = Cursor::new(wire.as_slice());

    let mut inflated = 0usize;
    let mut steps = 0usize;
    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(data) => {
                assert!(
                    data.len() <= step_limit,
                    "one decoder step yielded {} bytes, upstream caps a step at {step_limit}",
                    data.len()
                );
                assert!(data.iter().all(|&b| b == 0), "literal bytes corrupted");
                inflated += data.len();
                steps += 1;
            }
            CompressedToken::BlockMatch(idx) => panic!("unexpected block match {idx}"),
            CompressedToken::End => break,
        }
    }
    assert_eq!(
        inflated, INFLATED_LEN,
        "concatenated output must equal the original"
    );
    assert!(steps >= INFLATED_LEN / step_limit);
    assert_eq!(
        cursor.position() as usize,
        wire.len(),
        "whole stream consumed"
    );
}

/// Output must leave the decoder before the rest of the packet is inflated.
///
/// The packet holds a 64 MiB zero frame followed by a corrupt frame header.
/// Upstream returns every step of the good frame as a literal and only fails
/// (RERR_STREAMIO, token.c:899-904) when its step reaches the corrupt bytes.
/// A decoder that inflates the whole packet up front fails on the very first
/// call, having first buffered all 64 MiB.
#[test]
fn zstd_packet_output_is_emitted_before_the_rest_is_inflated() {
    let mut payload = zero_frame();
    payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0]);
    let wire = single_packet(&payload);
    let step_limit = upstream_out_buffer_size();
    let mut decoder = CompressedTokenDecoder::new_zstd().unwrap();
    let mut cursor = Cursor::new(wire.as_slice());

    let mut inflated = 0usize;
    let err = loop {
        match decoder.recv_token(&mut cursor) {
            Ok(CompressedToken::Literal(data)) => {
                assert!(data.len() <= step_limit);
                inflated += data.len();
            }
            Ok(other) => panic!("unexpected token before the corrupt frame: {other:?}"),
            Err(err) => break err,
        }
    };
    assert_eq!(
        inflated, INFLATED_LEN,
        "every step before the corrupt frame must be delivered first (error: {err})"
    );
}
