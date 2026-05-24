//! Wire-byte regression test for file-list sort-key selection at protocol 28
//! vs 29 (RP28.h).
//!
//! Upstream `flist.c:f_name_cmp()` picks its sort key at runtime via
//! `t_path = protocol_version >= 29 ? t_PATH : t_ITEM`
//! (upstream: `flist.c:3223` in `target/interop/upstream-src/rsync-3.4.1`).
//!
//! - `t_PATH` (protocol >= 29) introduces the file-before-directory rule at
//!   each path level and treats directories as if they carry an implicit
//!   trailing `/`. This is what makes a directory whose name is a prefix of a
//!   sibling file sort after that file.
//! - `t_ITEM` (protocol < 29) is a plain byte-for-byte comparison with no
//!   directory specialisation, identical to a sort on the raw name bytes.
//!
//! This test pins both halves of the gate at the wire-byte boundary: the
//! receiver-visible order of name bytes inside the serialised file list must
//! differ between protocol 28 and protocol 29 for a fixture engineered to
//! disambiguate the two comparators. A regression in
//! `sort_file_list(_, _, protocol_pre29)` that silently downgrades the v29
//! comparator to `t_ITEM` (or vice versa) would produce mismatching NDX
//! values on the receiver and silent data placement errors, so the assertion
//! lives at the serialised-bytes layer rather than as an in-memory order
//! check.
//!
//! Inventory cross-reference: item F1 in
//! `docs/design/rp28-a-pre30-code-paths-inventory.md`
//! ("Sort comparator (pre-29 vs 29+)").

use std::io::Cursor;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter, sort_file_list};

fn proto28() -> ProtocolVersion {
    ProtocolVersion::try_from(28u8).expect("protocol 28 is supported")
}

fn proto29() -> ProtocolVersion {
    ProtocolVersion::from_supported(29).expect("protocol 29 is supported")
}

/// Fixture chosen so the two comparators disagree on three independent axes:
///
/// 1. `dir` (directory) vs `dir.txt` (file) at the same level. Under `t_PATH`
///    the directory is compared as `dir/`; `/` (0x2F) > `.` (0x2E), so the
///    file sorts first. Under `t_ITEM` `dir` is a strict prefix of `dir.txt`
///    and sorts first.
/// 2. `aardvark` (directory) vs `zebra.txt` (file) at the same level. Under
///    `t_PATH` files sort before directories regardless of name, so the file
///    is first. Under `t_ITEM` byte order wins and the directory is first.
/// 3. `dot` is the directory `"."` and must lead under both comparators -
///    pinning that invariant on the wire as well prevents an accidental swap
///    of the dot-first short-circuit between the two paths.
fn fixture() -> Vec<FileEntry> {
    vec![
        FileEntry::new_file("zebra.txt".into(), 10, 0o644),
        FileEntry::new_directory("aardvark".into(), 0o755),
        FileEntry::new_file("dir.txt".into(), 20, 0o644),
        FileEntry::new_directory("dir".into(), 0o755),
        FileEntry::new_directory(".".into(), 0o755),
    ]
}

/// Reads back every name from a serialised file list and returns them in
/// wire order. Mirrors the receiver's view of the sort and pins the
/// regression at the wire-byte boundary rather than at an in-memory check.
fn names_on_wire(protocol: ProtocolVersion, entries: &[FileEntry]) -> Vec<String> {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in entries {
        writer.write_entry(&mut buf, entry).expect("write entry");
    }
    writer.write_end(&mut buf, None).expect("write end");

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read entry") {
        decoded.push(entry.name().to_string());
    }
    decoded
}

/// Protocol 28 must use the `t_ITEM` comparator: plain lexicographic byte
/// order with `"."` short-circuited to the front. Pinning the exact decoded
/// name sequence guards against any future change that silently routes the
/// pre-29 path through the `t_PATH` comparator.
#[test]
fn proto28_wire_order_matches_t_item() {
    let mut entries = fixture();
    sort_file_list(&mut entries, false, true);

    let on_wire = names_on_wire(proto28(), &entries);
    assert_eq!(
        on_wire,
        vec![
            ".",        // dot-first short-circuit (both comparators)
            "aardvark", // 'a' (0x61) < 'd' (0x64): plain byte order
            "dir",      // strict prefix of "dir.txt" under t_ITEM
            "dir.txt",
            "zebra.txt", // 'z' last under plain byte order
        ],
        "protocol 28 must serialise entries in t_ITEM (plain byte) order",
    );
}

/// Protocol 29 must use the `t_PATH` comparator: files sort before
/// directories at each level and directories carry an implicit trailing `/`.
/// Pinning the exact decoded name sequence guards against any future change
/// that silently routes the v29 path through the `t_ITEM` comparator.
#[test]
fn proto29_wire_order_matches_t_path() {
    let mut entries = fixture();
    sort_file_list(&mut entries, false, false);

    let on_wire = names_on_wire(proto29(), &entries);
    assert_eq!(
        on_wire,
        vec![
            ".",         // dot-first short-circuit
            "dir.txt",   // file "dir.txt" sorts before dir "dir/" since '.' < '/'
            "zebra.txt", // files before dirs at root level
            "aardvark",  // directory after both root files
            "dir",       // directory after "aardvark" by name
        ],
        "protocol 29 must serialise entries in t_PATH (file-before-dir) order",
    );
}

/// Direct wire-byte gate assertion: the same fixture serialised under the
/// two protocols must produce different byte streams when each is sorted
/// with the protocol-appropriate comparator. If a regression collapses the
/// two sort keys into one, both streams would coincide and the bug would
/// otherwise be invisible until an actual peer NDX mismatch.
#[test]
fn proto28_and_proto29_wire_streams_diverge() {
    let mut entries_pre29 = fixture();
    sort_file_list(&mut entries_pre29, false, true);
    let mut entries_v29 = fixture();
    sort_file_list(&mut entries_v29, false, false);

    let names_pre29 = names_on_wire(proto28(), &entries_pre29);
    let names_v29 = names_on_wire(proto29(), &entries_v29);

    assert_ne!(
        names_pre29, names_v29,
        "t_PATH and t_ITEM must produce different wire orderings for the \
         shared fixture; matching orderings indicate the protocol gate has \
         been bypassed",
    );
}
