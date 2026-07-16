//! Receiver-side duplicate-clean pass coverage.
//!
//! Upstream `flist.c:flist_sort_and_clean()` runs three steps after the
//! receiver decodes the file list: sort, drop duplicate names (keeping the
//! upstream-correct survivor by tombstoning the dropped slot in place), then
//! prune empty dirs. These tests pin the second step - a sender that emits the
//! same normalized name twice (redundant or hostile) must leave the dropped
//! entry as an inactive tombstone that preserves every NDX slot, never a
//! compacted/renumbered list.
//!
//! # Upstream Reference
//!
//! - `flist.c:3016 flist_sort_and_clean()` - the combined sort+dedup+prune pass
//!   run by `recv_file_list()` at `flist.c:2771`.

use std::io::Cursor;

use protocol::flist::{FileEntry, FileListWriter};

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// Encode a list of entries into a wire buffer terminated by the end marker.
fn encode(protocol: protocol::ProtocolVersion, entries: &[FileEntry]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();
    data
}

/// A file name emitted twice by the sender must be TOMBSTONED in place, not
/// compacted away.
///
/// WHY: upstream drops the duplicate via `clear_file()` on the dropped index
/// (`flist.c:3089`), which zeroes the entry but leaves its slot in
/// `flist->files[]` so every following NDX is unchanged. The receiver must
/// preserve the array length and every NDX slot; compacting/renumbering would
/// desync the receiver's numbering from the sender's full un-deduped array
/// (received "non-regular file" / silent corruption). The generator skips the
/// inactive slot, so the path is never requested twice.
#[test]
fn receiver_tombstones_duplicate_file_name() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let entries = [
        FileEntry::new_file("dup.txt".into(), 100, 0o644),
        FileEntry::new_file("dup.txt".into(), 100, 0o644),
        FileEntry::new_file("b.txt".into(), 50, 0o644),
    ];
    let data = encode(handshake.protocol, &entries);

    let mut cursor = Cursor::new(&data[..]);
    ctx.receive_file_list(&mut cursor).unwrap();

    // All three NDX slots are preserved; one is an inactive tombstone.
    assert_eq!(ctx.file_list().len(), 3);
    let active: Vec<_> = ctx
        .file_list()
        .iter()
        .filter(|e| e.is_active())
        .map(|e| e.name())
        .collect();
    assert_eq!(active, vec!["b.txt", "dup.txt"]);
    assert_eq!(
        ctx.file_list().iter().filter(|e| !e.is_active()).count(),
        1,
        "the repeated name leaves exactly one tombstone slot"
    );
}

/// Legitimately distinct names must all survive - no over-dedup.
///
/// WHY: the dedup key is the full normalized name; distinct paths that merely
/// share a prefix (or a basename in different dirs) are not duplicates and must
/// all reach the generator.
#[test]
fn receiver_keeps_distinct_names() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let entries = [
        FileEntry::new_directory("d".into(), 0o755),
        FileEntry::new_file("d/x.txt".into(), 1, 0o644),
        FileEntry::new_file("dx.txt".into(), 1, 0o644),
        FileEntry::new_file("a.txt".into(), 1, 0o644),
    ];
    let data = encode(handshake.protocol, &entries);

    let mut cursor = Cursor::new(&data[..]);
    ctx.receive_file_list(&mut cursor).unwrap();

    let names: Vec<_> = ctx.file_list().iter().map(|e| e.name()).collect();
    assert_eq!(names, vec!["a.txt", "dx.txt", "d", "d/x.txt"]);
}

/// When a file and a directory share a name, upstream keeps the directory.
///
/// WHY: `flist_sort_and_clean` keeps the dir "because it might have contents in
/// the list" (`flist.c:3060`). Dropping the dir in favour of the plain file
/// would orphan its children.
#[test]
fn receiver_keeps_directory_over_file_duplicate() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let entries = [
        FileEntry::new_file("item".into(), 10, 0o644),
        FileEntry::new_directory("item".into(), 0o755),
    ];
    let data = encode(handshake.protocol, &entries);

    let mut cursor = Cursor::new(&data[..]);
    ctx.receive_file_list(&mut cursor).unwrap();

    // Both NDX slots are preserved; the plain-file slot is tombstoned and the
    // directory survives so its NDX still matches the sender's dir entry.
    assert_eq!(ctx.file_list().len(), 2);
    let active: Vec<_> = ctx.file_list().iter().filter(|e| e.is_active()).collect();
    assert_eq!(active.len(), 1);
    assert!(
        active[0].is_dir(),
        "duplicate collision must keep the directory, not the plain file"
    );
}
