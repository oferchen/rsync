#![no_main]

//! Fuzz target for `NegotiationPrologueSniffer` pre-auth byte handling.
//!
//! The sniffer inspects the very first bytes of every incoming connection to
//! decide whether the peer is speaking the legacy `@RSYNCD:` ASCII greeting or
//! the binary protocol. Because it runs before authentication, malformed input
//! must never panic, over-allocate, or otherwise become a DoS vector.
//!
//! Covers the three public entry points:
//! * `observe_byte` - single-byte feed path.
//! * `observe` - bulk slice feed path.
//! * `read_from` - generic `Read` source path (driven by a `Cursor`).

use libfuzzer_sys::fuzz_target;
use protocol::NegotiationPrologueSniffer;

fuzz_target!(|data: &[u8]| {
    // observe_byte path: drip bytes one at a time.
    let mut sniffer = NegotiationPrologueSniffer::new();
    for &b in data {
        let _ = sniffer.observe_byte(b);
    }

    // observe path: hand the whole slice in one call.
    let mut sniffer_bulk = NegotiationPrologueSniffer::new();
    let _ = sniffer_bulk.observe(data);

    // read_from path: feed via a `Cursor<&[u8]>` so EOF is reachable.
    let mut cursor = std::io::Cursor::new(data);
    let mut sniffer_reader = NegotiationPrologueSniffer::new();
    let _ = sniffer_reader.read_from(&mut cursor);
});
