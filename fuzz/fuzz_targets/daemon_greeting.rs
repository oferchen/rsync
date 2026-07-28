#![no_main]

//! Fuzz target for the `@RSYNCD:` legacy daemon greeting parser.
//!
//! The greeting is the first line every daemon sends and every client
//! consumes, well before authentication. Any panic in this parser is a
//! pre-auth remote attack surface, so this target exercises the byte-level
//! entry points with arbitrary inputs. The byte variants delegate to the
//! string-based parser after UTF-8 validation, covering both branches via
//! the same call.
//!
//! Coverage spans all three public byte parsers so the fuzzer surfaces any
//! divergence between the protocol-only, detail, and owned forms:
//!
//! - [`parse_legacy_daemon_greeting_bytes`]
//! - [`parse_legacy_daemon_greeting_bytes_details`]
//! - [`parse_legacy_daemon_greeting_bytes_owned`]
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run daemon_greeting
//! ```

use libfuzzer_sys::fuzz_target;

use protocol::{
    advertised_digests_in_greeting, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details, parse_legacy_daemon_greeting_bytes_owned,
};

fuzz_target!(|data: &[u8]| {
    let _ = parse_legacy_daemon_greeting_bytes(data);

    if let Ok(details) = parse_legacy_daemon_greeting_bytes_details(data) {
        let _ = details.protocol();
        let _ = details.advertised_protocol();
        let _ = details.subprotocol();

        let advertised = details.advertised_digests();
        let _ = advertised.tokens().count();

        // Whether a digest list was advertised decides whether the peer is
        // authenticated or refused, so the parser and the presence gate must
        // never disagree about it - including for an advertised-but-empty list,
        // which upstream keeps distinct from an absent one
        // (clientserver.c:199-203). Both read the same
        // `strchr(buf + 9, ' ')` rule, and this pins them together on arbitrary
        // input rather than on the handful of shapes the unit tests name.
        if let Ok(text) = std::str::from_utf8(data) {
            assert_eq!(
                advertised.is_present(),
                advertised_digests_in_greeting(text).is_present(),
                "parsed greeting and presence gate disagree on {text:?}",
            );
        }
    }

    if let Ok(owned) = parse_legacy_daemon_greeting_bytes_owned(data) {
        let _ = owned.protocol();
        let _ = owned.advertised_protocol();
        let _ = owned.has_subprotocol();

        // The owned view is what the daemon stashes while the greeting buffer is
        // recycled; it must not quietly drop an empty list on the way.
        assert_eq!(
            owned.advertised_digests().is_present(),
            owned.has_digest_list(),
        );
    }
});
