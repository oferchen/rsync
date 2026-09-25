//! Range-scoped receiver post-passes (`*_in_range`).
//!
//! The streaming INC_RECURSE receiver will run the symlink, special,
//! hardlink, implied-parent and missing-args passes once per segment and then
//! reclaim that segment's heap, instead of one whole-list pass at the end.
//! Upstream has no end-of-run pass at all: `recv_generator()` creates each
//! node inline (generator.c:1948-2002 symlinks, :2031 specials, :1749-1755
//! missing-args sentinels) and resolves a hardlink follower through the
//! `prior_hlinks` gnum table (hlink.c:281-291) after `flist_free()` dropped
//! the leader's segment.
//!
//! These tests pin the two properties the per-segment driver depends on:
//! a range call touches only its own entries, and ranges that tile the list
//! produce exactly the tree one whole-list call produces - including a
//! hardlink whose leader's segment was reclaimed before the follower's
//! segment ran.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::{TestDeletionWriter, make_hlink_follower, make_hlink_leader, test_handshake};
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

/// Receiver with every post-pass enabled: `-lHDR --delete-missing-args`.
fn receiver(entries: Vec<FileEntry>, segment_starts: &[usize]) -> ReceiverContext {
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-lHDRe.".to_owned(),
        flags: ParsedServerFlags {
            links: true,
            hard_links: true,
            devices: true,
            specials: true,
            relative: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.file_selection.delete_missing_args = true;
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list = entries;
    // Flat start of each segment; the wire ndx component is irrelevant here.
    ctx.ndx_segments = segment_starts
        .iter()
        .map(|&start| (start, start as i32 + 1))
        .collect();
    ctx
}

fn sentinel(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_file(name.into(), 0, 0);
    entry.set_mode(0);
    entry
}

/// Runs every post-pass over `range`, in the batch driver's order.
fn run_range(ctx: &mut ReceiverContext, dest: &Path, range: std::ops::Range<usize>) {
    let mut w = TestDeletionWriter;
    ctx.ensure_relative_parents_in_range(range.clone(), dest, None);
    ctx.create_symlinks_in_range(range.clone(), dest, None, &mut w)
        .unwrap();
    ctx.create_specials_in_range(range.clone(), dest, None, &mut w)
        .unwrap();
    ctx.process_missing_args_sentinels_in_range(range.clone(), dest, None)
        .unwrap();
    ctx.create_hardlinks_in_range(range, dest, None, &mut w)
        .unwrap();
}

/// Runs every whole-list post-pass (the entry points the drivers call today).
fn run_whole(ctx: &mut ReceiverContext, dest: &Path) {
    let mut w = TestDeletionWriter;
    ctx.ensure_relative_parents(dest, None);
    ctx.create_symlinks(dest, None, &mut w).unwrap();
    ctx.create_specials(dest, None, &mut w).unwrap();
    ctx.process_missing_args_sentinels(dest, None).unwrap();
    ctx.create_hardlinks(dest, None, &mut w).unwrap();
}

/// A symlink in segment 1 is created by segment 1's range only.
#[test]
fn symlink_in_range_touches_only_its_segment() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    let ctx = receiver(
        vec![
            FileEntry::new_file("f0".into(), 1, 0o644),
            FileEntry::new_symlink("link".into(), 0o777, "target".into()),
        ],
        &[0, 1],
    );
    let mut w = TestDeletionWriter;

    ctx.create_symlinks_in_range(0..1, dest, None, &mut w)
        .unwrap();
    assert!(dest.join("link").symlink_metadata().is_err());

    ctx.create_symlinks_in_range(1..2, dest, None, &mut w)
        .unwrap();
    assert_eq!(
        std::fs::read_link(dest.join("link")).unwrap(),
        Path::new("target")
    );
}

/// A FIFO in segment 1 is created by segment 1's range only.
#[test]
fn special_in_range_touches_only_its_segment() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    let ctx = receiver(
        vec![
            FileEntry::new_file("f0".into(), 1, 0o644),
            FileEntry::new_fifo("pipe".into(), 0o640),
        ],
        &[0, 1],
    );
    let mut w = TestDeletionWriter;

    ctx.create_specials_in_range(0..1, dest, None, &mut w)
        .unwrap();
    assert!(dest.join("pipe").symlink_metadata().is_err());

    ctx.create_specials_in_range(1..2, dest, None, &mut w)
        .unwrap();
    assert!(
        std::fs::symlink_metadata(dest.join("pipe"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

/// A `--relative` implied parent of a segment-1 entry is made by segment 1's
/// range only.
#[test]
fn relative_parents_in_range_touch_only_their_segment() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    let ctx = receiver(
        vec![
            FileEntry::new_file("f0".into(), 1, 0o644),
            FileEntry::new_file("x/y/f1".into(), 1, 0o644),
        ],
        &[0, 1],
    );

    ctx.ensure_relative_parents_in_range(0..1, dest, None);
    assert!(!dest.join("x").exists());

    ctx.ensure_relative_parents_in_range(1..2, dest, None);
    assert!(dest.join("x/y").is_dir());
}

/// A `--delete-missing-args` sentinel in segment 1 deletes its destination
/// only when segment 1's range runs.
#[test]
fn missing_args_in_range_touch_only_their_segment() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    std::fs::write(dest.join("gone"), b"stale").unwrap();
    let ctx = receiver(
        vec![FileEntry::new_file("f0".into(), 1, 0o644), sentinel("gone")],
        &[0, 1],
    );

    ctx.process_missing_args_sentinels_in_range(0..1, dest, None)
        .unwrap();
    assert!(dest.join("gone").exists());

    ctx.process_missing_args_sentinels_in_range(1..2, dest, None)
        .unwrap();
    assert!(!dest.join("gone").exists());
}

/// A follower in segment 1 is linked by segment 1's range only.
#[test]
fn hardlink_in_range_touches_only_its_segment() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    std::fs::write(dest.join("lead"), b"data").unwrap();
    let mut ctx = receiver(
        vec![
            make_hlink_leader("lead", 4, 7),
            make_hlink_follower("follow", 4, 7),
        ],
        &[0, 1],
    );
    let mut w = TestDeletionWriter;

    ctx.create_hardlinks_in_range(0..1, dest, None, &mut w)
        .unwrap();
    assert!(!dest.join("follow").exists());

    ctx.create_hardlinks_in_range(1..2, dest, None, &mut w)
        .unwrap();
    assert_eq!(
        std::fs::metadata(dest.join("follow")).unwrap().ino(),
        std::fs::metadata(dest.join("lead")).unwrap().ino()
    );
}

/// Why: the per-segment driver reclaims a segment before later segments run,
/// so a follower must reach its leader through the tracker (upstream's
/// `prior_hlinks`), never through the leader's now-empty flist entry.
#[test]
fn cross_segment_hardlink_survives_leader_segment_reclaim() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    std::fs::create_dir_all(dest.join("a")).unwrap();
    std::fs::write(dest.join("a/lead"), b"data").unwrap();
    let entries = || {
        vec![
            make_hlink_leader("a/lead", 4, 7),
            FileEntry::new_file("a/other".into(), 1, 0o644),
            make_hlink_follower("b/follow", 4, 7),
        ]
    };
    let mut w = TestDeletionWriter;

    let mut ctx = receiver(entries(), &[0, 2]);
    ctx.create_hardlinks_in_range(0..2, dest, None, &mut w)
        .unwrap();
    ctx.reclaim_oldest_segment();
    assert_eq!(
        ctx.file_list[0].hardlink_idx(),
        None,
        "seg0 must be reclaimed"
    );
    ctx.create_hardlinks_in_range(2..3, dest, None, &mut w)
        .unwrap();
    assert_eq!(
        std::fs::metadata(dest.join("b/follow")).unwrap().ino(),
        std::fs::metadata(dest.join("a/lead")).unwrap().ino()
    );
    assert_eq!(ctx.flist_io_error, 0);

    // Control: if seg0's range never ran, nothing recorded the leader and its
    // reclaimed entry cannot supply it - the follower is left unlinked. This
    // proves the pass above resolved through the tracker, not the flist.
    let tmp2 = tempfile::tempdir().unwrap();
    let dest2 = tmp2.path();
    std::fs::create_dir_all(dest2.join("a")).unwrap();
    std::fs::write(dest2.join("a/lead"), b"data").unwrap();
    let mut ctx = receiver(entries(), &[0, 2]);
    ctx.reclaim_oldest_segment();
    ctx.create_hardlinks_in_range(2..3, dest2, None, &mut w)
        .unwrap();
    assert!(!dest2.join("b/follow").exists());
}

/// Why: the per-segment driver must reproduce the batch drivers' tree. One
/// whole-list call and per-segment calls that tile the same list (reclaiming
/// each finished segment, as the streaming driver will) must agree.
#[test]
fn whole_list_equals_concatenated_segment_ranges() {
    let entries = || {
        vec![
            make_hlink_leader("d1/lead", 4, 3),
            FileEntry::new_symlink("d1/ln".into(), 0o777, "lead".into()),
            FileEntry::new_fifo("d1/fifo".into(), 0o640),
            sentinel("gone"),
            make_hlink_follower("d2/f1", 4, 3),
            FileEntry::new_symlink("d2/deep/ln2".into(), 0o777, "../f1".into()),
            // Top-level: without bindat(2) a NESTED socket is skipped on
            // macOS/BSD (upstream: syscall.c:1508-1517 do_mknod_at EOPNOTSUPP,
            // generator.c:2506-2521), which would leave nothing to compare.
            FileEntry::new_socket("sock".into(), 0o600),
            make_hlink_follower("d3/f2", 4, 3),
            FileEntry::new_file("d3/e/f/plain".into(), 1, 0o644),
        ]
    };
    let seed = |dest: &Path| {
        std::fs::create_dir_all(dest.join("d1")).unwrap();
        std::fs::write(dest.join("d1/lead"), b"data").unwrap();
        std::fs::write(dest.join("gone"), b"stale").unwrap();
    };
    let starts = [0usize, 4, 7];

    let whole_dir = tempfile::tempdir().unwrap();
    seed(whole_dir.path());
    run_whole(&mut receiver(entries(), &starts), whole_dir.path());

    let seg_dir = tempfile::tempdir().unwrap();
    seed(seg_dir.path());
    let mut ctx = receiver(entries(), &starts);
    let bounds = [0usize, 4, 7, 9];
    for pair in bounds.windows(2) {
        run_range(&mut ctx, seg_dir.path(), pair[0]..pair[1]);
        ctx.reclaim_oldest_segment();
    }

    let whole = snapshot(whole_dir.path());
    assert_eq!(whole, snapshot(seg_dir.path()));
    // Guard against a vacuous match: every pass left a mark.
    assert_eq!(whole["d2/deep/ln2"], "l:../f1");
    assert_eq!(whole["d1/fifo"], "p");
    assert_eq!(whole["sock"], "s");
    assert!(whole["d3/e/f"].starts_with('d'));
    assert!(!whole.contains_key("gone"));
    assert_eq!(whole["d2/f1"], whole["d1/lead"]);
    assert_eq!(whole["d3/f2"], whole["d1/lead"]);
}

/// Relative path -> type tag; hardlinked regular files share a tag built from
/// the lowest path in their inode group, so link topology is compared too.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut raw = Vec::new();
    walk(root, root, &mut raw);
    let mut first_by_ino: BTreeMap<u64, String> = BTreeMap::new();
    for (rel, meta, _) in &raw {
        if meta.is_file() {
            first_by_ino
                .entry(meta.ino())
                .and_modify(|p| {
                    if rel < p {
                        p.clone_from(rel);
                    }
                })
                .or_insert_with(|| rel.clone());
        }
    }
    raw.into_iter()
        .map(|(rel, meta, link)| {
            let ft = meta.file_type();
            let tag = if ft.is_symlink() {
                format!("l:{}", link.unwrap())
            } else if ft.is_dir() {
                "d".to_owned()
            } else if ft.is_fifo() {
                "p".to_owned()
            } else if ft.is_socket() {
                "s".to_owned()
            } else {
                format!("f:{}", first_by_ino[&meta.ino()])
            };
            (rel, tag)
        })
        .collect()
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, std::fs::Metadata, Option<String>)>) {
    for dent in std::fs::read_dir(dir).unwrap() {
        let path = dent.unwrap().path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let link = meta.file_type().is_symlink().then(|| {
            std::fs::read_link(&path)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        });
        let is_dir = meta.is_dir();
        out.push((rel, meta, link));
        if is_dir {
            walk(root, &path, out);
        }
    }
}
