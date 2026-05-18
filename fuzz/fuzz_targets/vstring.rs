#![no_main]

//! Fuzz target for the vstring (variable-length string) parser surface.
//!
//! The vstring codec is exchanged during the protocol 30+ capability
//! negotiation, immediately after the compatibility-flag handshake and well
//! before authentication completes. Any panic in the reader is therefore a
//! pre-auth remote attack surface, which is why FCV-14 (#2441) called for
//! coverage-guided fuzzing on top of the FCV-3 audit findings (PR #4407).
//!
//! The byte-level `read_vstring` helper lives in
//! `crates/protocol/src/negotiation/capabilities/negotiate.rs` and is
//! restricted to `pub(super)` visibility. It is reached from outside the
//! crate exclusively through [`negotiate_capabilities`], which feeds the
//! caller-supplied reader into the vstring decoder. This target therefore
//! drives the reader through that public entry point, mirroring how an
//! unauthenticated peer reaches the parser on the wire.
//!
//! A selector byte at the head of the fuzz input chooses the protocol
//! version, role flags, and whether compression negotiation is active, so
//! libFuzzer can independently explore the one-byte and two-byte vstring
//! length encodings as well as the UTF-8 validation path. The remaining
//! bytes are handed to [`negotiate_capabilities`] as the peer's stream so
//! the vstring reader consumes them directly.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run vstring
//! ```
//!
//! Seed corpus lives at `fuzz/corpus/vstring/`.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::{ProtocolVersion, negotiate_capabilities};

fuzz_target!(|data: &[u8]| {
    let (selector, payload) = match data.split_first() {
        Some(split) => split,
        None => return,
    };

    // Cycle through every protocol version that performs vstring negotiation
    // (>= 30) plus a legacy version to exercise the short-circuit path that
    // returns defaults without touching the reader.
    let protocol = match selector & 0b11 {
        0 => ProtocolVersion::try_from(30),
        1 => ProtocolVersion::try_from(31),
        2 => ProtocolVersion::try_from(32),
        _ => ProtocolVersion::try_from(28),
    };
    let Ok(protocol) = protocol else { return };

    let do_negotiation = selector & 0b0000_0100 != 0;
    let send_compression = selector & 0b0000_1000 != 0;
    let is_daemon_mode = selector & 0b0001_0000 != 0;
    let is_server = selector & 0b0010_0000 != 0;

    // The reader is the peer's stream consumed by `read_vstring`. A sink
    // captures whatever the negotiator emits so the writer never errors and
    // we always reach the read path.
    let mut stdin = Cursor::new(payload);
    let mut stdout: Vec<u8> = Vec::new();

    let _ = negotiate_capabilities(
        protocol,
        &mut stdin,
        &mut stdout,
        do_negotiation,
        send_compression,
        is_daemon_mode,
        is_server,
    );
});
