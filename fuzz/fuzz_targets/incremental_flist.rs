#![no_main]

//! Fuzz target for the incremental file-list state machine.
//!
//! Drives [`StreamingFileList`] end-to-end with arbitrary wire bytes. The
//! state machine is reachable post-auth but pre-transfer: a malicious
//! peer that completes the daemon handshake can stream arbitrary file
//! list segments and observe the receiver's response. The state machine
//! must:
//!
//! 1. Decode each entry off the wire via `FileListReader::read_entry`.
//! 2. Track parent directory dependencies and release pending children
//!    once their parents are created.
//! 3. Drain ready entries via [`next_ready`](StreamingFileList::next_ready)
//!    in dependency order.
//! 4. Finalize orphans (entries whose parent was never received) without
//!    panicking.
//!
//! Both legacy (protocol 28) and INC_RECURSE (protocol 30 with
//! `CF_INC_RECURSE`) wire formats are exercised. Any panic from
//! decoding, dependency tracking, or finalization is a finding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run incremental_flist
//! ```

use std::io::Cursor;

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use protocol::flist::{FileListReader, IncrementalFileList, StreamingFileList};
use protocol::{CompatibilityFlags, ProtocolVersion};

/// Wire format mode selector. Mirrors the existing `flist_entry_decode`
/// target so the two corpora can be shared if useful.
#[derive(Arbitrary, Debug)]
enum Mode {
    Legacy,
    IncRecurse,
}

/// Preserve-flag toggles that change the wire byte budget per entry.
#[derive(Arbitrary, Debug)]
struct PreserveFlags {
    uid: bool,
    gid: bool,
    links: bool,
    devices: bool,
    specials: bool,
    hard_links: bool,
    atimes: bool,
    crtimes: bool,
}

#[derive(Arbitrary, Debug)]
struct Input {
    mode: Mode,
    flags: PreserveFlags,
    payload: Vec<u8>,
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(input) = Input::arbitrary(&mut u) else {
        // Even when the structured wrapper fails, drive the raw bytes
        // through the state machine under both protocol modes so libFuzzer
        // gets coverage on the plain-byte entry point too.
        drive_raw(data);
        return;
    };

    let (protocol, compat) = match input.mode {
        Mode::Legacy => (ProtocolVersion::V28, CompatibilityFlags::EMPTY),
        Mode::IncRecurse => (ProtocolVersion::V30, CompatibilityFlags::INC_RECURSE),
    };

    let reader = FileListReader::with_compat_flags(protocol, compat)
        .with_preserve_uid(input.flags.uid)
        .with_preserve_gid(input.flags.gid)
        .with_preserve_links(input.flags.links)
        .with_preserve_devices(input.flags.devices)
        .with_preserve_specials(input.flags.specials)
        .with_preserve_hard_links(input.flags.hard_links)
        .with_preserve_atimes(input.flags.atimes)
        .with_preserve_crtimes(input.flags.crtimes);

    let cursor = Cursor::new(input.payload.as_slice());
    let mut streaming = match input.mode {
        Mode::Legacy => StreamingFileList::new(reader, cursor),
        Mode::IncRecurse => StreamingFileList::with_incremental_recursion(reader, cursor),
    };

    // Drain ready entries until the wire is exhausted or hits a parse
    // error. The state machine must not panic on any byte sequence.
    let mut budget = 4096usize;
    while budget > 0 {
        match streaming.next_ready() {
            Ok(Some(_entry)) => budget -= 1,
            Ok(None) => break,
            Err(_) => break,
        }
    }

    // Also exercise the stand-alone state machine's `finalize` path,
    // which walks the orphan-resolution code and synthesizes placeholder
    // directories. Even an empty processor must finalize cleanly.
    let incremental = IncrementalFileList::new();
    let _ = incremental.finalize();

    drive_raw(data);
});

/// Hammers the streaming state machine with raw fuzzer bytes under both
/// protocol versions, with no preserve flags toggled.
fn drive_raw(data: &[u8]) {
    for (protocol, compat) in [
        (ProtocolVersion::V28, CompatibilityFlags::EMPTY),
        (ProtocolVersion::V30, CompatibilityFlags::INC_RECURSE),
    ] {
        let reader = FileListReader::with_compat_flags(protocol, compat);
        let cursor = Cursor::new(data);
        let mut streaming = StreamingFileList::new(reader, cursor);

        let mut budget = 1024usize;
        while budget > 0 {
            match streaming.next_ready() {
                Ok(Some(_)) => budget -= 1,
                Ok(None) | Err(_) => break,
            }
        }
    }
}
