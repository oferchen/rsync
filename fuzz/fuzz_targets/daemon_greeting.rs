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
    parse_legacy_daemon_greeting_bytes, parse_legacy_daemon_greeting_bytes_details,
    parse_legacy_daemon_greeting_bytes_owned,
};

fuzz_target!(|data: &[u8]| {
    let _ = parse_legacy_daemon_greeting_bytes(data);

    if let Ok(details) = parse_legacy_daemon_greeting_bytes_details(data) {
        let _ = details.protocol();
        let _ = details.advertised_protocol();
        let _ = details.subprotocol();
        let _ = details.digest_list();
    }

    if let Ok(owned) = parse_legacy_daemon_greeting_bytes_owned(data) {
        let _ = owned.protocol();
        let _ = owned.advertised_protocol();
        let _ = owned.has_subprotocol();
    }
});
