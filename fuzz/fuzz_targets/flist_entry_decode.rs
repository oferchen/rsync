#![no_main]

//! Fuzz target for the rsync file-list entry decoder.
//!
//! `recv_file_entry` (upstream: `flist.c:recv_file_entry()`) is the most
//! complex parser in the protocol: a single byte stream that carries XMIT
//! flag bits, run-length-encoded path suffixes, varint-encoded sizes, and
//! optionally interned hardlink / ACL / xattr indexes. The wire format
//! diverges between legacy (`protocol < 30`) and INC_RECURSE-capable
//! (`protocol >= 30`) sessions, and many preserve flags toggle additional
//! trailing fields.
//!
//! This target uses [`Arbitrary`] to flip between the two wire-format
//! "modes" and to randomise the preserve flag matrix so libFuzzer can
//! explore every conditional branch in the decoder. Errors are expected on
//! malformed inputs; only panics constitute a finding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run flist_entry_decode
//! ```

use std::io::Cursor;

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use protocol::flist::{FileListReader, read_file_entry};
use protocol::{CompatibilityFlags, ProtocolVersion};

/// Wire format mode selector. Legacy mode pins the reader to protocol 28
/// (pre-INC_RECURSE). INC_RECURSE mode pins it to protocol 30 with the
/// `CF_INC_RECURSE` capability flag asserted so the decoder takes the
/// segmented-list path.
#[derive(Arbitrary, Debug)]
enum Mode {
    Legacy,
    IncRecurse,
}

/// Random preserve-flag matrix. Each toggle gates a trailing field in the
/// wire encoding, so flipping them changes the byte budget the decoder
/// expects per entry.
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
    checksum_len: u8,
    acls: bool,
    xattrs: bool,
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
        // Even without a structured wrap, hammer the convenience function
        // with the raw bytes under both protocols.
        decode_raw(data);
        return;
    };

    let (protocol, compat) = match input.mode {
        Mode::Legacy => (ProtocolVersion::V28, CompatibilityFlags::EMPTY),
        Mode::IncRecurse => (ProtocolVersion::V30, CompatibilityFlags::INC_RECURSE),
    };

    let mut reader = FileListReader::with_compat_flags(protocol, compat)
        .with_preserve_uid(input.flags.uid)
        .with_preserve_gid(input.flags.gid)
        .with_preserve_links(input.flags.links)
        .with_preserve_devices(input.flags.devices)
        .with_preserve_specials(input.flags.specials)
        .with_preserve_hard_links(input.flags.hard_links)
        .with_preserve_atimes(input.flags.atimes)
        .with_preserve_crtimes(input.flags.crtimes)
        .with_always_checksum(usize::from(input.flags.checksum_len % 33))
        .with_preserve_acls(input.flags.acls)
        .with_preserve_xattrs(input.flags.xattrs);

    let mut cursor = Cursor::new(input.payload.as_slice());
    // Drain entries until end-of-list or first parse error - the decoder
    // must not panic for any byte sequence.
    while let Ok(Some(_entry)) = reader.read_entry(&mut cursor) {}

    // Also feed the raw fuzzer bytes through the convenience function so
    // libFuzzer rewards coverage of the stateless entry-point path.
    decode_raw(data);
});

/// Hammer the stateless [`read_file_entry`] helper against the raw input
/// under both legacy and INC_RECURSE protocol versions.
fn decode_raw(data: &[u8]) {
    for protocol in [ProtocolVersion::V28, ProtocolVersion::V30] {
        let mut cursor = Cursor::new(data);
        let _ = read_file_entry(&mut cursor, protocol);
    }
}
