//! Per-segment parent directory and its content flag under INC_RECURSE.
//!
//! Upstream runs the per-directory delete for a sub-list's parent when that
//! sub-list becomes `cur_flist` (`generator.c:2780-2801`). It finds the parent
//! through `cur_flist->parent_ndx`, a `dir_flist` index, and sweeps only when
//! the parent carries `FLAG_CONTENT_DIR`. These tests pin the two facts the
//! receiver records for that consumer, through the real receive path:
//!
//! - the parent index, including the first list's `-1` rule
//!   (`flist.c:3071-3083`), since a wrong `Some(0)` points the delete at a
//!   directory the list never described;
//! - the content flag as it stands AFTER the implied-parent downgrade
//!   (`flist.c:1240-1256`), since the sender's claim is what lets a hostile
//!   peer widen `--delete` to siblings the client merely traversed.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use protocol::CompatibilityFlags;
use protocol::codec::NDX_FLIST_OFFSET;
use protocol::flist::{FileEntry, FileListWriter};

use super::super::super::ReceiverContext;
use super::super::super::file_list::DirSlot;
use super::super::support::{test_config, test_handshake};
use crate::config::ServerConfig;

fn encode_entries(entries: &[FileEntry]) -> Vec<u8> {
    let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();
    data
}

fn inc_recurse_receiver(config: ServerConfig) -> ReceiverContext {
    let mut handshake = test_handshake();
    handshake.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
    ReceiverContext::new_for_test(&handshake, config)
}

fn dir(name: &str) -> FileEntry {
    FileEntry::new_directory(PathBuf::from(name), 0o755)
}

fn file(name: &str) -> FileEntry {
    let mut e = FileEntry::new_file(PathBuf::from(name), 10, 0o100644);
    e.set_mtime(1_700_000_000, 0);
    e
}

fn receive_initial(ctx: &mut ReceiverContext, entries: &[FileEntry]) {
    ctx.receive_file_list(&mut Cursor::new(encode_entries(entries)))
        .expect("initial list must be accepted");
}

fn receive_sub_list(ctx: &mut ReceiverContext, dir_ndx: i32, entries: &[FileEntry]) {
    ctx.receive_one_extra_segment(
        &mut Cursor::new(encode_entries(entries)),
        NDX_FLIST_OFFSET - dir_ndx,
        0,
    )
    .expect("sub-list must be accepted");
}

/// The slot's content flag, or a panic naming what the slot actually is.
fn slot_content_dir(ctx: &ReceiverContext, dir_ndx: i32, expected_name: &str) -> bool {
    match ctx.dir_flist.resolve(dir_ndx) {
        Some(DirSlot::Active { name, content_dir }) => {
            assert_eq!(name, Path::new(expected_name), "slot {dir_ndx} name");
            *content_dir
        }
        other => panic!("slot {dir_ndx} is not active: {other:?}"),
    }
}

/// A `.`-rooted first list has `dir_flist` slot 0 as its parent, and each
/// sub-list records its header's `dir_ndx` - the `dir_flist` index, not the
/// directory's position in the transfer list.
#[test]
fn dot_rooted_list_and_sub_lists_record_dir_flist_parents() {
    let mut ctx = inc_recurse_receiver(test_config());
    receive_initial(&mut ctx, &[dir("."), file("a.txt"), dir("d"), dir("e")]);
    assert_eq!(ctx.segment_parent_dir_ndx, vec![Some(0)]);

    receive_sub_list(&mut ctx, 2, &[file("e/y.txt")]);
    receive_sub_list(&mut ctx, 1, &[file("d/x.txt")]);

    assert_eq!(
        ctx.segment_parent_dir_ndx,
        vec![Some(0), Some(2), Some(1)],
        "a sub-list's parent is its header dir_ndx, in arrival order"
    );
    assert_eq!(ctx.segment_parent_dir_ndx.len(), ctx.ndx_segments.len());
}

/// A first list not rooted at `.` (e.g. `rsync -r host:d dest`, whose lowest
/// entry is `d`) has no parent: upstream sets `parent_ndx = -1`, so no
/// directory is swept for it even though `dir_flist` slot 0 exists.
#[test]
fn non_dot_rooted_list_has_no_parent() {
    let mut ctx = inc_recurse_receiver(test_config());
    receive_initial(&mut ctx, &[dir("d"), file("d/x.txt")]);

    assert_eq!(ctx.dir_flist.used(), 1, "`d` occupies slot 0");
    assert_eq!(ctx.segment_parent_dir_ndx, vec![None]);
}

/// `-R host:a/b/c` implies parents `a` and `a/b`. A sender that flags them as
/// content dirs must not have that claim survive into `dir_flist`, or the
/// per-directory delete would sweep siblings the client never asked for.
fn relative_implied_config() -> ServerConfig {
    let mut config = test_config();
    config.flags.recursive = true;
    config.flags.relative = true;
    config.connection.implied_source_args = vec![b"a/b/c".to_vec()];
    config
}

/// The initial list's slots are appended before the pipeline setup runs the
/// downgrade, so the downgrade itself must reach back into `dir_flist`.
#[test]
fn initial_list_slot_takes_the_post_downgrade_content_flag() {
    let mut ctx = inc_recurse_receiver(relative_implied_config());
    receive_initial(&mut ctx, &[dir("a"), dir("a/b"), dir("a/b/c")]);
    assert!(
        slot_content_dir(&ctx, 0, "a"),
        "fixture: the sender claimed `a` as a content dir"
    );

    // The pipeline setup's pass (build_pipeline_setup).
    ctx.downgrade_implied_parent_dirs().unwrap();

    assert!(!slot_content_dir(&ctx, 0, "a"), "implied parent `a`");
    assert!(!slot_content_dir(&ctx, 1, "a/b"), "implied parent `a/b`");
    assert!(slot_content_dir(&ctx, 2, "a/b/c"), "the requested dir");
}

/// A sub-list's directories are downgraded before their slots are appended,
/// so the slot records the downgraded flag directly.
#[test]
fn sub_list_slot_takes_the_post_downgrade_content_flag() {
    let mut ctx = inc_recurse_receiver(relative_implied_config());
    receive_initial(&mut ctx, &[dir("a")]);
    ctx.downgrade_implied_parent_dirs().unwrap();

    receive_sub_list(&mut ctx, 0, &[dir("a/b")]);
    receive_sub_list(&mut ctx, 1, &[dir("a/b/c")]);

    assert!(!slot_content_dir(&ctx, 1, "a/b"), "implied parent `a/b`");
    assert!(slot_content_dir(&ctx, 2, "a/b/c"), "the requested dir");
    assert_eq!(
        ctx.segment_parent_dir_ndx,
        vec![None, Some(0), Some(1)],
        "a list rooted at `a`, not `.`, has no parent"
    );
}
