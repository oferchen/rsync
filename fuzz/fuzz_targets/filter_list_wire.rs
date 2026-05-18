#![no_main]

//! Fuzz target for the post-auth filter-list wire decoder.
//!
//! After authentication completes and before file-list transfer begins, the
//! receiver feeds its filter rule list to the sender. The wire format - 4-byte
//! length-prefixed records terminated by a zero-length record - is parsed by
//! [`protocol::filters::wire::read_filter_list`] (upstream:
//! `exclude.c:recv_filter_list()`). Any panic here is reachable by an
//! authenticated peer, so we fuzz the parser against all five supported
//! protocol versions.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run filter_list_wire
//! ```

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::ProtocolVersion;
use protocol::filters::wire::read_filter_list;

fuzz_target!(|data: &[u8]| {
    // Exercise the parser at every supported wire revision so libFuzzer can
    // explore the old-prefix (protocol 28) and modern-prefix (29+) branches
    // plus the protocol-gated modifier flags.
    for version in 28u8..=32 {
        let Some(protocol) = ProtocolVersion::from_supported(version) else {
            continue;
        };
        let mut cursor = Cursor::new(data);
        let _ = read_filter_list(&mut cursor, protocol);
    }
});
