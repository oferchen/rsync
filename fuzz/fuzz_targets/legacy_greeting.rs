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
//! validated UTF-8.
//!
//! # Oracle
//!
//! The six entry points are convenience wrappers around one core parser, so
//! they must never disagree on the same input. Beyond panic-freedom this
//! target pins them together:
//!
//! - The byte parsers delegate to the string parsers after a *lossy* UTF-8
//!   conversion, so on valid UTF-8 input the byte and string variants must
//!   reach the identical verdict (same protocol version, or both reject).
//! - The protocol-only, detail, and owned string variants must agree on the
//!   negotiated protocol and the advertised protocol number - a divergence
//!   would mean one wrapper reads a different field than another.
//!
//! This catches a wrong-but-non-panicking parse that a discard-the-result
//! target could never see.
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
    let bytes_protocol = parse_legacy_daemon_greeting_bytes(data);
    let _ = parse_legacy_daemon_greeting_bytes_details(data);
    let _ = parse_legacy_daemon_greeting_bytes_owned(data);

    // String-oriented entry points: callers that have already validated the
    // banner as UTF-8 use these. Only feed valid UTF-8 to keep the fuzz
    // path under test rather than the `from_utf8` check itself.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let str_protocol = parse_legacy_daemon_greeting(text).ok();
    let details = parse_legacy_daemon_greeting_details(text);
    let owned = parse_legacy_daemon_greeting_owned(text);

    // On valid UTF-8 the byte parser is a pure pass-through to the string
    // parser, so the two must reach the same verdict. Compare successful
    // protocol versions; a rejection on either side collapses to `None`.
    assert_eq!(
        bytes_protocol.ok(),
        str_protocol,
        "byte and string legacy-greeting parsers disagree on {text:?}",
    );

    // The protocol-only wrapper is defined as `_details(..).map(protocol)`,
    // so whenever the detailed parse succeeds the two must agree, and the
    // owned view must carry the identical negotiated and advertised numbers.
    match (details, owned) {
        (Ok(details), Ok(owned)) => {
            assert_eq!(
                str_protocol,
                Some(details.protocol()),
                "detail and protocol-only parsers disagree on {text:?}",
            );
            assert_eq!(
                owned.protocol(),
                details.protocol(),
                "owned and borrowed greeting disagree on protocol for {text:?}",
            );
            assert_eq!(
                owned.advertised_protocol(),
                details.advertised_protocol(),
                "owned and borrowed greeting disagree on advertised protocol for {text:?}",
            );
        }
        (Err(_), Err(_)) => {}
        (details, owned) => panic!(
            "detail and owned greeting parsers disagree on success for {text:?}: \
             details_ok={} owned_ok={}",
            details.is_ok(),
            owned.is_ok(),
        ),
    }
});
