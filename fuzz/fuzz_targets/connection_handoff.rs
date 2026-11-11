#![no_main]

//! Fuzz target for the Windows daemon's connection hand-off frame.
//!
//! The frame crosses from the daemon to a per-connection session process,
//! which must refuse any malformed frame without panicking or
//! over-allocating. Any frame that decodes must re-encode to the same bytes:
//! the encoding is canonical, and a mismatch means a field was silently
//! altered on the way through.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run connection_handoff
//! ```

use libfuzzer_sys::fuzz_target;

use daemon::connection_handoff::ConnectionHandoff;

fuzz_target!(|data: &[u8]| {
    if let Ok(handoff) = ConnectionHandoff::decode(data) {
        let encoded = handoff.encode().expect("a decoded frame re-encodes");
        assert_eq!(encoded, data, "the encoding must be canonical");
    }
    let _ = ConnectionHandoff::read_from(&mut &data[..]);
});
