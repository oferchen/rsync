//! Receiver-side decoding of the `--crtimes` (`-N`) file-list field.
//!
//! upstream: `flist.c:743-746` - `recv_file_entry()` reads a crtime varlong
//! whenever `crtimes_ndx` is set and `XMIT_CRTIME_EQ_MTIME` is clear. The field
//! sits between the mtime block and the mode, so a receiver that does not
//! consume it parses the crtime bytes as the mode and every following field
//! shifts.

use std::ffi::OsString;
use std::io::Cursor;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListWriter};

use super::super::super::ReceiverContext;
use super::super::support::test_handshake;
use crate::config::ServerConfig;
use crate::role::ServerRole;

const PROTOCOL: u8 = 32;

/// A `-N` receiver: `--times --crtimes`, matching the flag string the sender
/// is handed in `oc-rsync -rtN`.
fn crtimes_config() -> ServerConfig {
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
        flag_string: "-rtNe.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.flags.recursive = true;
    config.flags.times = true;
    config.flags.crtimes = true;
    config
}

/// Encodes `entries` the way a `--crtimes` sender does.
fn crtimes_wire_bytes(entries: &[FileEntry]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::try_from(PROTOCOL).unwrap())
        .with_preserve_crtimes(true);
    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();
    data
}

/// A crtime that differs from the mtime puts a varlong on the wire
/// (`XMIT_CRTIME_EQ_MTIME` is only set when the two are equal,
/// `flist.c:XMIT_CRTIME_EQ_MTIME`). The receiver must consume it.
///
/// Why it matters: without `with_preserve_crtimes` on the receiver's reader,
/// those bytes were parsed as the mode field and everything after it shifted.
/// A real `oc-rsync -rtN` pull from an upstream 3.4.4 sender died with
/// "received file entry with zero-length filename" (exit 12) for any file
/// whose birth time was not its mtime; upstream transferred it fine.
#[test]
fn crtime_differing_from_mtime_does_not_desync_the_file_list() {
    let mut first = FileEntry::new_file("alpha.txt".into(), 42, 0o644);
    first.set_mtime(1_893_448_800, 0);
    first.set_crtime(1_600_000_000);
    let mut second = FileEntry::new_file("beta.txt".into(), 7, 0o600);
    second.set_mtime(1_893_448_800, 0);
    second.set_crtime(1_500_000_000);
    let data = crtimes_wire_bytes(&[first, second]);

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, crtimes_config());
    let count = ctx
        .receive_file_list(&mut Cursor::new(data))
        .expect("a -N file list must decode without desynchronising");

    assert_eq!(count, 2);
    let list = ctx.file_list();
    let names: Vec<&str> = list.iter().map(FileEntry::name).collect();
    assert_eq!(names, vec!["alpha.txt", "beta.txt"]);
    // The fields *after* the crtime are the ones a missed read corrupts.
    assert_eq!(
        list[0].mode() & 0o7777,
        0o644,
        "mode shifted past the crtime"
    );
    assert_eq!(list[0].size(), 42, "size shifted past the crtime");
    assert_eq!(
        list[0].crtime(),
        1_600_000_000,
        "the crtime itself must reach the entry so itemize() can compare it"
    );
    assert_eq!(list[1].crtime(), 1_500_000_000);
}

/// The equal-crtime case sends no varlong (`XMIT_CRTIME_EQ_MTIME`), so it
/// decoded correctly even before the fix. Pin it so the reader keeps
/// reconstructing `crtime == mtime` rather than leaving zero, which would make
/// every up-to-date file report a spurious `n` column.
#[test]
fn crtime_equal_to_mtime_is_reconstructed_from_the_mtime() {
    let mut entry = FileEntry::new_file("gamma.txt".into(), 3, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_crtime(1_700_000_000);
    let data = crtimes_wire_bytes(&[entry]);

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, crtimes_config());
    ctx.receive_file_list(&mut Cursor::new(data)).unwrap();

    assert_eq!(ctx.file_list()[0].crtime(), 1_700_000_000);
}
