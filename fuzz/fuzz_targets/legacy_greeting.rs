#![no_main]

//! Fuzz target for the legacy ASCII daemon greeting / version-negotiation
//! parser.
//!
//! The `@RSYNCD: <protocol>[.<sub>] [digests]` banner is the very first line
//! every legacy (pre-protocol-30) peer exchanges, on both the client and
//! server side. Both directions feed the received bytes through the same
//! family of parsers exposed by the [`protocol`] crate. A panic in any of
//! them is reachable over the network before authentication, so each public
//! entry point is fuzzed here.
//!
//! The target exercises both the byte-oriented entry points
//! ([`parse_legacy_daemon_greeting_bytes`], `..._bytes_details`,
//! `..._bytes_owned`) used when the input comes straight off the wire, and
//! the string-oriented variants ([`parse_legacy_daemon_greeting`],
//! `..._details`, `..._owned`) used by higher layers that have already
//! validated UTF-8. The owned/details/protocol-only entry points exist as
//! convenience wrappers around the same core parser; fuzzing each one
//! independently lets libFuzzer catch divergences between them.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run legacy_greeting
//! ```
//!
//! Seed corpus lives at `fuzz/corpus/legacy_greeting/`.

use libfuzzer_sys::fuzz_target;

use protocol::{
    parse_legacy_daemon_greeting, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details, parse_legacy_daemon_greeting_bytes_owned,
    parse_legacy_daemon_greeting_details, parse_legacy_daemon_greeting_owned,
};

fuzz_target!(|data: &[u8]| {
    // Byte-oriented entry points: the exact functions that consume bytes
    // straight off the network on both client and server sides.
    let _ = parse_legacy_daemon_greeting_bytes(data);
    let _ = parse_legacy_daemon_greeting_bytes_details(data);
    let _ = parse_legacy_daemon_greeting_bytes_owned(data);

    // String-oriented entry points: callers that have already validated the
    // banner as UTF-8 use these. Only feed valid UTF-8 to keep the fuzz
    // path under test rather than the `from_utf8` check itself.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = parse_legacy_daemon_greeting(text);
        let _ = parse_legacy_daemon_greeting_details(text);
        let _ = parse_legacy_daemon_greeting_owned(text);
    }
});
