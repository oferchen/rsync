//! Input-space robustness tests for the highest-risk wire-protocol decoders.
//!
//! A decoder that panics, reads out of bounds, over-allocates, or loops forever
//! on attacker-controlled wire bytes is a denial-of-service vector. Upstream
//! rsync has shipped exactly this class of bug (out-of-bounds reads and integer
//! overflows in the file-list and compressed-token paths). These tests feed
//! arbitrary and adversarial byte streams into the decode entry points and
//! assert only that decoding *terminates gracefully* - it returns `Ok` or a
//! typed `Err`, never panics, never allocates without bound, never spins.
//!
//! Scope note: the individual file-list field decoders (`decode_flags`,
//! `decode_name`, ...) and the multiplex/varint decoders already have arbitrary
//! -byte panic-freedom sweeps elsewhere in this crate. This file covers the two
//! gaps: the *whole* file-list entry decoder driven end to end, and a broad
//! arbitrary-byte sweep over the compressed-token decoder (whose crafted
//! integer-overflow regressions live inline in the token module).

use std::io::Cursor;

use proptest::prelude::*;

use protocol::flist::{EntryStep, FileEntry, FileListReader};
use protocol::wire::{CompressedToken, CompressedTokenDecoder};
use protocol::{ProtocolVersion, write_varint30_int};

/// Protocol versions the file-list decoder must survive arbitrary input on.
const PROTOCOL_VERSIONS: [u8; 5] = [28, 29, 30, 31, 32];

fn protocol(version: u8) -> ProtocolVersion {
    ProtocolVersion::try_from(version).expect("supported protocol version")
}

// --- File-list entry decode (CVE-2026-43620 class: crafted flist) ------------
//
// Upstream CVE-2026-43620 was an out-of-bounds read reachable from a maliciously
// crafted incremental file list (a first entry that is not the implied dot-dir,
// an index of zero where a positive follower index was required, transfer flags
// without the expected item bit). At the decode layer the exposure is a single
// `FileListReader` entry decode fed hostile bytes: an over-long or negative
// name/suffix length, a truncated entry, or garbage flag combinations. The
// decoder must reject every such input with an error instead of panicking or
// allocating from an unchecked wire length.

mod flist_entry_decode {
    use super::*;

    // Wire flag bits, upstream rsync.h `XMIT_*`. The `flags` module is private,
    // so the values are pinned here (they are part of the stable wire format).
    const XMIT_EXTENDED_FLAGS: u8 = 1 << 2;
    const XMIT_SAME_NAME: u8 = 1 << 5;
    const XMIT_LONG_NAME: u8 = 1 << 6;

    /// Drives one entry decode over `bytes` for every protocol version through
    /// both the blocking reader and the sans-io step seam. Returns nothing; the
    /// contract is simply that neither entry point panics.
    fn decode_all_paths(bytes: &[u8]) {
        for version in PROTOCOL_VERSIONS {
            let mut reader = FileListReader::new(protocol(version));
            let mut cursor = Cursor::new(bytes);
            // Result is intentionally discarded: Ok(Some)/Ok(None)/Err are all
            // acceptable graceful outcomes. A panic here fails the test.
            let _ = reader.read_entry(&mut cursor);

            let mut step_reader = FileListReader::new(protocol(version));
            match step_reader.read_entry_step(bytes, &[]) {
                Ok(EntryStep::Emit { consumed, .. }) => {
                    assert!(
                        consumed <= bytes.len(),
                        "read_entry_step reported consuming {consumed} of {} bytes",
                        bytes.len()
                    );
                }
                Ok(EntryStep::NeedMore) | Err(_) => {}
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// The whole file-list entry decoder never panics on arbitrary bytes.
        #[test]
        fn read_entry_never_panics_on_arbitrary_input(
            data in prop::collection::vec(any::<u8>(), 0..8192),
        ) {
            decode_all_paths(&data);
        }

        /// Arbitrary bytes prefixed with a "same name" reuse byte exercise the
        /// prefix-compression path (`same_len` against an empty previous name)
        /// without panicking - a raw `any::<u8>()` prefix rarely sets that flag.
        #[test]
        fn read_entry_never_panics_with_same_name_prefix(
            same_len in any::<u8>(),
            data in prop::collection::vec(any::<u8>(), 0..4096),
        ) {
            let mut bytes = Vec::with_capacity(data.len() + 2);
            bytes.push(XMIT_SAME_NAME);
            bytes.push(same_len);
            bytes.extend_from_slice(&data);
            decode_all_paths(&bytes);
        }
    }

    #[test]
    fn empty_input_needs_more_and_end_marker_is_none() {
        // A zero-length flag byte is the end-of-list marker; an empty buffer is
        // just "need more". Both are graceful, neither allocates.
        let mut reader = FileListReader::new(protocol(32));
        assert!(matches!(
            reader.read_entry_step(&[], &[]).expect("empty step"),
            EntryStep::NeedMore
        ));

        let mut reader = FileListReader::new(protocol(32));
        let mut cursor = Cursor::new([0u8].as_slice());
        assert!(
            reader
                .read_entry(&mut cursor)
                .expect("end marker decodes")
                .is_none()
        );
    }

    #[test]
    fn oversize_long_name_length_is_rejected_not_allocated() {
        // A malicious sender sets XMIT_LONG_NAME and encodes a near-i32::MAX
        // suffix length. The decoder must reject it via the MAXPATHLEN bound
        // (name.rs) before allocating gigabytes, on every protocol version.
        for version in PROTOCOL_VERSIONS {
            let mut bytes = vec![XMIT_LONG_NAME];
            write_varint30_int(&mut bytes, i32::MAX, version).expect("encode length");
            // No name bytes follow: the guard must fire before the read_exact.
            let mut reader = FileListReader::new(protocol(version));
            let mut cursor = Cursor::new(bytes.as_slice());
            let result = reader.read_entry(&mut cursor);
            assert!(
                result.is_err(),
                "proto {version}: oversize long-name length must error, got {result:?}"
            );
        }
    }

    #[test]
    fn negative_long_name_length_is_rejected() {
        // A negative wire length (high bit set) must not underflow into a huge
        // usize allocation. Protocol >= 30 uses varint; encode -1 and confirm a
        // clean error rather than a panic or runaway allocation.
        for version in [30u8, 31, 32] {
            let mut bytes = vec![XMIT_LONG_NAME];
            write_varint30_int(&mut bytes, -1, version).expect("encode length");
            let mut reader = FileListReader::new(protocol(version));
            let mut cursor = Cursor::new(bytes.as_slice());
            let result = reader.read_entry(&mut cursor);
            assert!(
                result.is_err(),
                "proto {version}: negative long-name length must error, got {result:?}"
            );
        }
    }

    #[test]
    fn truncated_entries_never_panic() {
        // Every truncation of a valid entry must surface as an error (or a
        // NeedMore at the step seam), never a panic. Build a real entry, then
        // feed every strict prefix of it.
        use protocol::flist::FileListWriter;

        let version = 32;
        let mut full = Vec::new();
        let mut writer = FileListWriter::new(protocol(version));
        let mut entry = FileEntry::new_file("dir/some-file".into(), 4096, 0o100644);
        entry.set_mtime(1_700_000_000, 0);
        writer
            .write_entry(&mut full, &entry)
            .expect("write baseline entry");

        for cut in 0..full.len() {
            let prefix = &full[..cut];
            let mut reader = FileListReader::new(protocol(version));
            let mut cursor = Cursor::new(prefix);
            // Only assertion: the call returns (no panic). Ok/Err both fine.
            let _ = reader.read_entry(&mut cursor);

            let mut step_reader = FileListReader::new(protocol(version));
            let _ = step_reader.read_entry_step(prefix, &[]);
        }
    }

    #[test]
    fn adversarial_flag_bytes_never_panic() {
        // A grab-bag of hostile leading flag bytes across all protocols: the
        // extended-flags marker with nothing after it, long+same name combined,
        // and every single-bit flag in isolation followed by no payload.
        let mut cases: Vec<Vec<u8>> = vec![
            vec![XMIT_EXTENDED_FLAGS],
            vec![XMIT_EXTENDED_FLAGS, 0xFF],
            vec![XMIT_LONG_NAME | XMIT_SAME_NAME],
            vec![XMIT_SAME_NAME, 0xFF],
            vec![0xFF],
            vec![0xFF, 0xFF, 0xFF, 0xFF],
        ];
        for bit in 0..8u8 {
            cases.push(vec![1u8 << bit]);
        }
        for bytes in &cases {
            decode_all_paths(bytes);
        }
    }
}

// --- Compressed-token decode (CVE-2026-43618 class: integer overflow) --------
//
// Upstream CVE-2026-43618 was an integer overflow in the compressed-token
// counter: a run of relative tokens could wrap the running index past INT_MAX
// and drive an out-of-bounds access. The counter guard (`checked_add` in the
// token step machine) has crafted regression coverage inline in the token
// module. This adds the missing dimension - a broad arbitrary-byte sweep over
// the public `recv_token` decode loop - proving the whole decoder terminates
// gracefully on any input rather than panicking or spinning.

mod compressed_token_decode {
    use super::*;

    /// Ceiling on tokens a decoder may emit *without consuming any input* before
    /// it is deemed a non-terminating spin.
    ///
    /// A single `TOKENRUN_REL` / `TOKENRUN_LONG` flag legitimately schedules up
    /// to 65535 (a 16-bit run count) `BlockMatch` tokens, each drained by a
    /// `recv_token` call that reads no further wire bytes - this is upstream
    /// token.c behaviour, bounded, and not a DoS. Any run of emissions longer
    /// than that with the reader position frozen is a genuine infinite loop.
    const STALL_LIMIT: usize = 70_000;

    /// Absolute safety net on total `recv_token` calls for one buffer. Legitimate
    /// run amplification is bounded by the input length (each run costs three
    /// input bytes); reaching this bound just means the arbitrary bytes encoded a
    /// very long but finite run sequence, so draining stops cleanly - panic
    /// freedom over this many calls is what the sweep set out to prove.
    const CALL_LIMIT: usize = 100_000;

    /// Drains tokens from `decoder` over `bytes` until `End`, an error, or the
    /// call limit. The contract: `recv_token` never panics, and never emits more
    /// than [`STALL_LIMIT`] tokens in a row without advancing the reader (which
    /// would be a spin). A bounded long run that reaches [`CALL_LIMIT`] is a pass.
    fn drain(mut decoder: CompressedTokenDecoder, bytes: &[u8]) {
        let mut cursor = Cursor::new(bytes);
        let mut last_pos = cursor.position();
        let mut stall = 0usize;
        for _ in 0..CALL_LIMIT {
            match decoder.recv_token(&mut cursor) {
                Ok(CompressedToken::End) | Err(_) => return,
                Ok(CompressedToken::Literal(_)) | Ok(CompressedToken::BlockMatch(_)) => {
                    let pos = cursor.position();
                    if pos == last_pos {
                        stall += 1;
                        assert!(
                            stall <= STALL_LIMIT,
                            "recv_token emitted {stall} tokens without consuming input: spin"
                        );
                    } else {
                        stall = 0;
                        last_pos = pos;
                    }
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The zlib token decoder never panics or spins on arbitrary bytes.
        #[test]
        fn zlib_recv_token_never_panics_on_arbitrary_input(
            data in prop::collection::vec(any::<u8>(), 0..4096),
        ) {
            drain(CompressedTokenDecoder::new(), &data);
        }

        /// The zlibx token decoder (no per-block dictionary) likewise survives.
        #[test]
        fn zlibx_recv_token_never_panics_on_arbitrary_input(
            data in prop::collection::vec(any::<u8>(), 0..4096),
        ) {
            let mut decoder = CompressedTokenDecoder::new();
            decoder.set_zlibx(true);
            drain(decoder, &data);
        }
    }

    #[cfg(feature = "zstd")]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The zstd token decoder never panics or spins on arbitrary bytes.
        #[test]
        fn zstd_recv_token_never_panics_on_arbitrary_input(
            data in prop::collection::vec(any::<u8>(), 0..4096),
        ) {
            drain(CompressedTokenDecoder::new_zstd().expect("zstd decoder"), &data);
        }
    }

    #[cfg(feature = "lz4")]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The lz4 token decoder never panics or spins on arbitrary bytes.
        #[test]
        fn lz4_recv_token_never_panics_on_arbitrary_input(
            data in prop::collection::vec(any::<u8>(), 0..4096),
        ) {
            drain(CompressedTokenDecoder::new_lz4(), &data);
        }
    }

    #[test]
    fn truncated_and_degenerate_streams_terminate() {
        // Deterministic edge cases: empty input, a lone flag byte, all-zero and
        // all-one buffers of various lengths. Each must terminate (Err/End),
        // never spin.
        let cases: [Vec<u8>; 6] = [
            Vec::new(),
            vec![0x00],
            vec![0xFF],
            vec![0x00; 512],
            vec![0xFF; 512],
            (0..=255u8).cycle().take(2048).collect(),
        ];
        for bytes in cases {
            drain(CompressedTokenDecoder::new(), &bytes);
        }
    }
}
