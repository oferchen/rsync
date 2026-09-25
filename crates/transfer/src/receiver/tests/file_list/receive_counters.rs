//! Receive-time `--stats` counters: per-type tallies and the total size.
//!
//! Upstream bumps `stats.num_dirs` / `num_symlinks` / `num_devices` /
//! `num_specials` in `recv_file_list()`'s read loop (`flist.c:3236-3249`) and
//! `stats.total_size` in `recv_file_entry()` (`flist.c:1613-1614`), i.e. as
//! each entry arrives and before `flist_sort_and_clean()` tombstones anything.
//! The receiver mirrors that so the figures survive the release of completed
//! INC_RECURSE segments and count exactly what upstream counts.

use std::io::Cursor;
use std::path::PathBuf;

use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::flist::{FileEntry, FileListWriter, FileType};
use protocol::{CompatibilityFlags, ProtocolVersion};

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// Receiver preserving links, devices and specials, so every type decodes.
fn receiver(compat: Option<CompatibilityFlags>, prune_empty_dirs: bool) -> ReceiverContext {
    let mut handshake = test_handshake();
    handshake.compat_flags = compat;
    let mut config = test_config();
    config.flags.links = true;
    config.flags.devices = true;
    config.flags.specials = true;
    config.flags.prune_empty_dirs = prune_empty_dirs;
    ReceiverContext::new_for_test(&handshake, config)
}

fn writer(protocol: ProtocolVersion) -> FileListWriter {
    FileListWriter::new(protocol)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true)
}

fn write_entries(wire: &mut Vec<u8>, writer: &mut FileListWriter, entries: &[FileEntry]) {
    for entry in entries {
        let mut e = entry.clone();
        e.set_mtime(1_700_000_000, 0);
        writer.write_entry(wire, &e).unwrap();
    }
    writer.write_end(wire, None).unwrap();
}

fn sized(mut entry: FileEntry, size: u64) -> FileEntry {
    entry.set_size(size);
    entry
}

fn dir(name: &str) -> FileEntry {
    // A non-zero directory st_size, as a real sender ships, so a total that
    // wrongly included directories would show it.
    sized(FileEntry::new_directory(PathBuf::from(name), 0o755), 96)
}

fn file(name: &str, size: u64) -> FileEntry {
    FileEntry::new_file(PathBuf::from(name), size, 0o644)
}

fn symlink(name: &str, target: &str) -> FileEntry {
    let len = target.len() as u64;
    sized(
        FileEntry::new_symlink(PathBuf::from(name), 0o777, PathBuf::from(target)),
        len,
    )
}

/// The pre-counter derivation: a walk over whatever the list holds right now.
fn walk(list: &[FileEntry]) -> ((u64, u64, u64, u64), u64) {
    let mut counts = (0, 0, 0, 0);
    let mut total = 0;
    for e in list {
        if e.is_dir() {
            counts.0 += 1;
        } else if e.is_symlink() {
            counts.1 += 1;
        } else if e.is_device() {
            counts.2 += 1;
        } else if e.is_special() {
            counts.3 += 1;
        }
        if matches!(e.file_type(), FileType::Regular | FileType::Symlink) {
            total += e.size();
        }
    }
    (counts, total)
}

/// Releasing completed INC_RECURSE segments must not change `--stats`.
///
/// WHY: the receiver frees finished sub-list segments mid-transfer (upstream
/// `flist_free()`, receiver.c:699), zeroing their entries. A figure derived by
/// walking the list at the end would then count every freed entry as nothing
/// and print a smaller "Number of files" breakdown and "Total file size" than
/// upstream, whose counters were bumped as the entries arrived.
#[test]
fn stats_survive_reclaimed_segments() {
    let protocol = test_handshake().protocol;
    let mut ctx = receiver(Some(CompatibilityFlags::INC_RECURSE), false);

    // Segment 0 (initial list): dir_flist becomes [".", "a", "b"].
    let mut w = writer(protocol);
    let mut initial = Vec::new();
    write_entries(
        &mut initial,
        &mut w,
        &[
            dir("."),
            dir("a"),
            dir("b"),
            symlink("l", "target"),
            file("g", 5),
        ],
    );
    ctx.receive_file_list(&mut Cursor::new(initial)).unwrap();

    // Segments 1 and 2: the sub-lists of "a" (dir_ndx 1) and "b" (dir_ndx 2).
    let mut codec = create_ndx_codec(protocol.as_u8());
    let mut subs = Vec::new();
    codec.write_ndx(&mut subs, NDX_FLIST_OFFSET - 1).unwrap();
    write_entries(&mut subs, &mut w, &[file("a/x", 10), dir("a/sub")]);
    codec.write_ndx(&mut subs, NDX_FLIST_OFFSET - 2).unwrap();
    write_entries(
        &mut subs,
        &mut w,
        &[
            file("b/y", 20),
            FileEntry::new_fifo(PathBuf::from("b/p"), 0o644),
            FileEntry::new_char_device(PathBuf::from("b/c"), 0o600, 1, 3),
        ],
    );
    codec.write_ndx(&mut subs, NDX_FLIST_EOF).unwrap();
    ctx.receive_extra_file_lists(&mut Cursor::new(subs))
        .unwrap();
    assert_eq!(
        ctx.ndx_segments.len(),
        3,
        "fixture must span three segments"
    );

    let (truth_counts, truth_total) = walk(ctx.file_list());
    assert_eq!(truth_counts, (4, 1, 1, 1), "dirs . a b a/sub; l; b/c; b/p");
    assert_eq!(
        truth_total,
        5 + 6 + 10 + 20,
        "regular files plus the symlink"
    );

    ctx.reclaim_oldest_segment();
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.first_segment_idx, 2, "two segments must be reclaimed");
    assert_ne!(
        walk(ctx.file_list()),
        (truth_counts, truth_total),
        "reclaim must actually zero entries, or this test proves nothing"
    );

    assert_eq!(ctx.file_type_counts(), truth_counts);
    assert_eq!(ctx.total_source_size(), truth_total);
}

/// Duplicates the clean later tombstones are still counted.
///
/// WHY: upstream counts in the read loop, before `flist_sort_and_clean()`
/// clears a duplicate. Measured against rsync 3.5.1: pulling `src/d src/d
/// src/g src/g` (`d` holding a 3-byte `f`, `g` 2 bytes) with
/// `--no-inc-recursive` reports `Number of files: 6 (reg: 4, dir: 2)` and
/// `Total file size: 10 bytes`. Counting after the clean would print `dir: 1`
/// and `5 bytes`, and push the lost directory into the `reg` remainder.
#[test]
fn duplicates_are_counted_as_received() {
    let protocol = test_handshake().protocol;
    let mut ctx = receiver(None, false);
    let mut w = writer(protocol);
    let mut wire = Vec::new();
    write_entries(
        &mut wire,
        &mut w,
        &[
            dir("d"),
            file("d/f", 3),
            dir("d"),
            file("d/f", 3),
            file("g", 2),
            file("g", 2),
        ],
    );
    let received = ctx.receive_file_list(&mut Cursor::new(wire)).unwrap();

    assert_eq!(received, 6);
    assert_eq!(
        ctx.file_list().iter().filter(|e| !e.is_active()).count(),
        3,
        "the clean must have tombstoned the three repeats"
    );
    assert_eq!(ctx.file_type_counts(), (2, 0, 0, 0));
    assert_eq!(ctx.total_source_size(), 10);
}

/// A directory `--prune-empty-dirs` clears is still counted as a directory.
///
/// WHY: the prune runs inside `flist_sort_and_clean()`, after the read loop
/// counted the directory. Measured against rsync 3.5.1: a pull of `d/f`, empty
/// `e/` and `g` with `-r --prune-empty-dirs` reports `dir: 3` although `e` is
/// never created.
#[test]
fn pruned_directory_is_counted_as_received() {
    let protocol = test_handshake().protocol;
    let mut ctx = receiver(None, true);
    let mut w = writer(protocol);
    let mut wire = Vec::new();
    write_entries(
        &mut wire,
        &mut w,
        &[dir("."), dir("d"), file("d/f", 3), dir("e"), file("g", 2)],
    );
    ctx.receive_file_list(&mut Cursor::new(wire)).unwrap();

    assert!(
        ctx.file_list().iter().any(|e| !e.is_active()),
        "the prune must have cleared the empty directory"
    );
    assert_eq!(ctx.file_type_counts(), (3, 0, 0, 0));
    assert_eq!(ctx.total_source_size(), 5);
}

/// On an ordinary list the counters equal the old end-of-transfer walk.
///
/// WHY: with no duplicate, no pruned directory and no reclaimed segment the
/// receive-time counters must print byte-identical `--stats` to the walk they
/// replace; any drift here would be a behaviour change, not a refactor.
#[test]
fn counters_match_list_walk_without_inc_recurse() {
    let protocol = test_handshake().protocol;
    let mut ctx = receiver(None, false);
    let mut w = writer(protocol);
    let mut wire = Vec::new();
    write_entries(
        &mut wire,
        &mut w,
        &[
            dir("."),
            dir("a"),
            file("a/x", 7),
            symlink("a/l", "x"),
            FileEntry::new_fifo(PathBuf::from("p"), 0o644),
            FileEntry::new_block_device(PathBuf::from("blk"), 0o600, 8, 0),
            file("z", 11),
        ],
    );
    ctx.receive_file_list(&mut Cursor::new(wire)).unwrap();

    let (counts, total) = walk(ctx.file_list());
    assert_eq!(counts, (2, 1, 1, 1));
    assert_eq!(ctx.file_type_counts(), counts);
    assert_eq!(ctx.total_source_size(), total);
}
