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
//! the decoder accepts must survive `write_filter_list` followed by a second
//! `read_filter_list` unchanged. A divergence means the encoder and decoder
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

        // Re-encode oracle: an accepted list must round-trip through the
        // writer and back without loss at the same protocol version. A write
        // error is not a finding: the decoder may accept rules the sender
        // would refuse to emit at this version (upstream: exclude.c:1623-1627
        // exits RERR_PROTOCOL for rules too modern for the negotiated wire).
        let mut wire = Vec::new();
        if write_filter_list(&mut wire, &rules, protocol).is_err() {
            continue;
        }
        let reread = read_filter_list(&mut Cursor::new(wire.as_slice()), protocol)
            .expect("re-reading our own filter list encoding must not fail");
        assert_eq!(
            rules, reread,
            "filter list wire round-trip divergence at protocol {version}",
        );
    }
});
