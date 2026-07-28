#![no_main]

//! Fuzz target for the post-auth filter-list wire decoder.
//!
//! After authentication completes and before file-list transfer begins, the
//! receiver feeds its filter rule list to the sender. The wire format - 4-byte
//! length-prefixed records terminated by a zero-length record - is parsed by
//! [`protocol::filters::read_filter_list`] (upstream:
//! `exclude.c:recv_filter_list()`). Any panic here is reachable by an
//! authenticated peer, so we fuzz the parser against all five supported
//! protocol versions.
//!
//! Beyond panic-freedom, the target asserts a re-encode oracle: any rule list
//! the decoder accepts must reach a fixed point after one
//! `write_filter_list` / `read_filter_list` normalization round. Divergence
//! or a decode failure on our own encoding means the encoder and decoder
//! disagree about the wire format, which would corrupt filter semantics
//! between peers.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run filter_list_wire
//! ```

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::ProtocolVersion;
use protocol::filters::{read_filter_list, write_filter_list};

fuzz_target!(|data: &[u8]| {
    // Exercise the parser at every supported wire revision so libFuzzer can
    // explore the old-prefix (protocol 28) and modern-prefix (29+) branches
    // plus the protocol-gated modifier flags.
    for version in 28u8..=32 {
        let Some(protocol) = ProtocolVersion::from_supported(version) else {
            continue;
        };
        let mut cursor = Cursor::new(data);
        let Ok(rules) = read_filter_list(&mut cursor, protocol) else {
            continue;
        };

        // Re-encode oracle: what the decoder accepts must re-encode and
        // re-decode to a fixed point at the same protocol version. Literal
        // equality with the first decode is too strict because upstream
        // normalizes some received forms (e.g. "P foo" is stored as exclude
        // + receiver-side and re-sent as "-r foo", exclude.c:1536-1572), so
        // require idempotence after one normalization round instead. A write
        // error is not a finding: the decoder may accept rules the sender
        // would refuse to emit at this version (upstream: exclude.c:1623-1627
        // exits RERR_PROTOCOL for rules too modern for the negotiated wire).
        let mut wire = Vec::new();
        if write_filter_list(&mut wire, &rules, protocol).is_err() {
            continue;
        }
        let once = read_filter_list(&mut Cursor::new(wire.as_slice()), protocol)
            .expect("re-reading our own filter list encoding must not fail");
        let mut wire_again = Vec::new();
        if write_filter_list(&mut wire_again, &once, protocol).is_err() {
            continue;
        }
        let twice = read_filter_list(&mut Cursor::new(wire_again.as_slice()), protocol)
            .expect("re-reading our own filter list encoding must not fail");
        assert_eq!(
            once, twice,
            "filter list wire encode/decode not idempotent at protocol {version}",
        );
    }
});
