#![no_main]

//! Fuzz target for the protocol capability-flags parser surface.
//!
//! The compatibility-flag bitfield is exchanged immediately after protocol
//! version negotiation and well before authentication. Any panic in the
//! decoders is therefore a pre-auth remote attack surface.
//!
//! Per the FCV-3 audit (PR #4407), the `negotiate_capabilities` / compat-flags
//! parser is flagged as a pre-auth gap that needs coverage-guided fuzzing.
//! `negotiate_capabilities` itself drives the full I/O exchange and is not a
//! pure parser; the byte-level entry points it ultimately consumes live on
//! [`CompatibilityFlags`] and adjacent pre-auth helpers. This target exercises
//! every parser an unauthenticated peer can reach by sending crafted bytes:
//!
//! - [`CompatibilityFlags::read_from`] - varint reader off the wire.
//! - [`CompatibilityFlags::decode_from_slice`] - slice-based variant.
//! - [`CompatibilityFlags::decode_from_slice_mut`] - cursor variant.
//! - [`KnownCompatibilityFlag::from_str`] - canonical `CF_*` identifier parser.
//! - [`detect_negotiation_prologue`] - pre-handshake byte sniff.
//! - [`NegotiationPrologue::from_str`] - identifier parser used by tooling.
//!
//! A selector byte at the head of the fuzz input chooses which path the
//! remaining bytes feed, so libFuzzer can independently explore each parser.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run capability_flags
//! ```
//!
//! Seed corpus lives at `fuzz/corpus/capability_flags/`.

use std::io::Cursor;
use std::str::FromStr;

use libfuzzer_sys::fuzz_target;

use protocol::{
    CompatibilityFlags, KnownCompatibilityFlag, NegotiationPrologue, detect_negotiation_prologue,
};

fuzz_target!(|data: &[u8]| {
    // Always feed the prologue detector - it operates on raw bytes with no
    // length precondition, which matches how it is invoked off the wire.
    let _ = detect_negotiation_prologue(data);

    // Always exercise the slice decoders so libFuzzer reaches them even when
    // the selector byte routes elsewhere.
    let _ = CompatibilityFlags::decode_from_slice(data);
    let mut slice: &[u8] = data;
    let _ = CompatibilityFlags::decode_from_slice_mut(&mut slice);

    let (selector, rest) = match data.split_first() {
        Some((head, tail)) => (*head, tail),
        None => return,
    };

    match selector % 4 {
        0 => {
            // Streaming varint decode off an in-memory cursor - mirrors the
            // I/O path used after protocol version exchange.
            let mut cursor = Cursor::new(rest);
            let _ = CompatibilityFlags::read_from(&mut cursor);
        }
        1 => {
            // Canonical `CF_*` identifier parser used by configuration and
            // diagnostic surfaces that surface flag names to operators.
            if let Ok(text) = std::str::from_utf8(rest) {
                let _ = KnownCompatibilityFlag::from_str(text);
            }
        }
        2 => {
            // Prologue identifier parser used by tooling that round-trips the
            // sniffed state through text. Mirrors the `Display` output.
            if let Ok(text) = std::str::from_utf8(rest) {
                let _ = NegotiationPrologue::from_str(text);
            }
        }
        _ => {
            // Round-trip every recoverable bit pattern back through the
            // decoder so encoder/decoder divergence surfaces as a panic.
            if rest.len() >= 4 {
                let bits = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
                let flags = CompatibilityFlags::from_bits(bits);
                let mut encoded = Vec::new();
                flags
                    .encode_to_vec(&mut encoded)
                    .expect("encode_to_vec into Vec cannot fail");
                let (decoded, remainder) = CompatibilityFlags::decode_from_slice(&encoded)
                    .expect("encoded bitfield must decode");
                assert_eq!(decoded, flags, "compatibility flags round-trip diverged");
                assert!(remainder.is_empty(), "decoder left trailing bytes");
            }
        }
    }
});
