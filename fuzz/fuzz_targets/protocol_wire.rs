#![no_main]

//! Top-level fuzz target for the rsync wire protocol parser.
//!
//! Feeds arbitrary bytes from libFuzzer into the highest-level multiplex
//! frame decoder. The decoder accepts untrusted bytes from network peers,
//! so any panic discovered here represents a potential remote attack
//! surface. Coverage-guided exploration takes care of fanning the input
//! out across the underlying header, payload-length, and message-code
//! validation paths.
//!
//! Additional targets (varint, file list, delta, filter rules, ...)
//! belong in sibling files under `fuzz/fuzz_targets/`.

use libfuzzer_sys::fuzz_target;

use protocol::BorrowedMessageFrames;

fuzz_target!(|data: &[u8]| {
    // Walk every frame in the buffer so libFuzzer explores both the
    // single-frame and multi-frame parser states. Errors are expected on
    // malformed input - only panics constitute a finding.
    for frame in BorrowedMessageFrames::new(data) {
        match frame {
            Ok(frame) => {
                let _ = frame.code();
                let _ = frame.payload_len();
                let _ = frame.payload();
            }
            Err(_) => break,
        }
    }
});
