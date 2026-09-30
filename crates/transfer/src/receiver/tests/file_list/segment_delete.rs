//! Per-directory delete over INC_RECURSE segments (`delete_in_segment`).
//!
//! Under INC_RECURSE a directory's names arrive in its own sub-list, possibly
//! long after the walk started. Upstream deletes in a directory only when that
//! sub-list becomes `cur_flist` and probes only that list
//! (generator.c:2780-2798, generator.c:347), so a name is never judged before
//! its directory's complete list is in hand. These tests pin that scope, the
//! gates upstream applies before `delete_in_dir()` touches a directory, and the
//! run-wide state the per-directory deletes share: one `--max-delete` budget
//! reported once (generator.c:2904-2909), one `NDX_DEL_STATS` tally, and the
//! sticky `IOERR_GENERAL` guard (generator.c:304-311).
//!
//! Every fixture is the same four-segment tree: segment 0 is the root's
//! sub-list (`.`, `a`, `b`, `c`), and segments 1-3 hold `a/k`, `b/k` and `c/k`,
//! each parented on its directory's `dir_flist` slot.

use std::ffi::OsString;
use std::path::Path;

use protocol::CompatibilityFlags;
use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::super::directory::FailedDirectories;
use super::super::super::file_list::DirFlist;
use super::super::super::stats::TransferStats;
use super::super::super::transfer::DeletePassPhase;
use super::super::support::{TestDeletionWriter, test_config, test_handshake};
use crate::config::ServerConfig;
use crate::generator::io_error_flags::{IOERR_DEL_LIMIT, IOERR_GENERAL};

const DIRS: [&str; 3] = ["a", "b", "c"];

/// Destination with `k` (listed) and `x` (extraneous) in every directory, plus
/// an extraneous `x` at the root.
fn populate(dest: &Path) {
    std::fs::write(dest.join("x"), b"x").unwrap();
    for dir in DIRS {
        std::fs::create_dir(dest.join(dir)).unwrap();
        std::fs::write(dest.join(dir).join("k"), b"k").unwrap();
        std::fs::write(dest.join(dir).join("x"), b"x").unwrap();
    }
}

/// `--delete-during` receiver config for `dest`, adjusted by `tweak`.
fn config(dest: &Path, tweak: impl FnOnce(&mut ServerConfig)) -> ServerConfig {
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.as_os_str())];
    tweak(&mut config);
    config
}

/// Receiver holding the four segments of the fixture tree.
fn receiver(config: ServerConfig) -> ReceiverContext {
    let mut handshake = test_handshake();
    handshake.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = vec![FileEntry::new_directory(".".into(), 0o755)];
    ctx.file_list.extend(
        DIRS.iter()
            .map(|dir| FileEntry::new_directory((*dir).into(), 0o755)),
    );
    ctx.file_list.extend(
        DIRS.iter()
            .map(|dir| FileEntry::new_file(format!("{dir}/k").into(), 1, 0o644)),
    );
    ctx.ndx_segments = vec![(0, 1), (4, 6), (5, 8), (6, 10)];
    ctx.segment_parent_dir_ndx = vec![Some(0), Some(1), Some(2), Some(3)];
    ctx.dir_flist = DirFlist::with_active([".", "a", "b", "c"]);
    ctx
}

/// Runs `delete_in_segment` for `segment`, with nothing failed.
fn delete_segment(
    ctx: &mut ReceiverContext,
    segment: usize,
    dest: &Path,
    stats: &mut TransferStats,
) -> std::io::Result<()> {
    ctx.delete_in_segment(
        segment,
        dest,
        #[cfg(unix)]
        None,
        &FailedDirectories::new(),
        &mut TestDeletionWriter,
        stats,
    )
}

fn exists(dest: &Path, rel: &str) -> bool {
    dest.join(rel).symlink_metadata().is_ok()
}

/// Counts drained Info events whose message is `text`.
fn count_info(text: &str) -> usize {
    logging::drain_events()
        .into_iter()
        .filter(|event| {
            matches!(event, logging::DiagnosticEvent::Info { message, .. } if message == text)
        })
        .count()
}

/// Walking only the root's sub-list deletes the root's extraneous entry and
/// nothing inside `a`, `b` or `c`, whose own sub-lists decide them later. A
/// keep-set wider than the one sub-list would see those directories with no
/// children and condemn their listed `k` files.
#[test]
fn root_segment_deletes_only_in_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |_| {}));
    let mut stats = TransferStats::default();

    delete_segment(&mut ctx, 0, dest, &mut stats).unwrap();

    assert!(!exists(dest, "x"), "the root's extraneous entry is deleted");
    for dir in DIRS {
        assert!(exists(dest, &format!("{dir}/k")), "{dir}/k is listed");
        assert!(
            exists(dest, &format!("{dir}/x")),
            "{dir}/x waits for {dir}'s own sub-list"
        );
    }
}

/// A name protects only the entry of the same directory: an `x` the segment
/// lists under another directory does not shield `a/x`.
///
/// upstream: generator.c:347 `flist_find_ignore_dirness()` compares full
/// names through `f_name_cmp()`, dirname included.
#[test]
fn a_same_named_entry_of_another_directory_protects_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |_| {}));
    ctx.file_list
        .insert(5, FileEntry::new_file("c/x".into(), 1, 0o644));
    ctx.ndx_segments = vec![(0, 1), (4, 6), (6, 9), (7, 11)];
    let mut stats = TransferStats::default();

    delete_segment(&mut ctx, 1, dest, &mut stats).unwrap();

    assert!(!exists(dest, "a/x"), "c/x must not protect a/x");
    assert!(exists(dest, "a/k"));
}

/// Each segment deletes in its own parent only, and the counts accumulate into
/// the single tally the goodbye `NDX_DEL_STATS` frame carries.
///
/// upstream: delete.c:241-256 bumps one global `stats.deleted_*`, written once
/// by main.c:228-240 `write_del_stats()`.
#[test]
fn segment_deletions_accumulate_into_one_tally() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |_| {}));
    let mut stats = TransferStats::default();

    for segment in 0..4 {
        delete_segment(&mut ctx, segment, dest, &mut stats).unwrap();
    }

    for dir in DIRS {
        assert!(exists(dest, &format!("{dir}/k")));
        assert!(!exists(dest, &format!("{dir}/x")));
    }
    assert_eq!(ctx.effective_del_stats().files, 4);
    assert_eq!(stats.io_error, 0);
}

/// `--max-delete=2` over three directories with one extraneous file each:
/// the cap spans the segments, so exactly two are deleted, the third is
/// counted as skipped, and nothing is reported until the walk ends - then one
/// warning and `IOERR_DEL_LIMIT`.
///
/// upstream: delete.c:217-218 tests the run-wide `stats.deleted_files`, and
/// generator.c:2904-2909 reports `skipped_deletes` once after every deletion.
#[test]
fn max_delete_budget_spans_segments_and_reports_once() {
    logging::init(logging::VerbosityConfig::from_verbose_level(0));
    let _ = logging::drain_events();
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    for dir in DIRS {
        std::fs::create_dir(dest.join(dir)).unwrap();
        std::fs::write(dest.join(dir).join("x"), b"x").unwrap();
    }
    let mut ctx = receiver(config(dest, |c| c.deletion.max_delete = Some(2)));
    let mut stats = TransferStats::default();

    for segment in 0..4 {
        delete_segment(&mut ctx, segment, dest, &mut stats).unwrap();
    }

    let survivors: Vec<_> = DIRS
        .iter()
        .filter(|dir| exists(dest, &format!("{dir}/x")))
        .collect();
    assert_eq!(survivors, [&"c"], "the cap stops the third deletion");
    assert_eq!(ctx.skipped_deletes, 1);
    let warning = "Deletions stopped due to --max-delete limit (1 skipped)";
    assert_eq!(count_info(warning), 0, "nothing is reported mid-walk");
    assert_eq!(stats.io_error & IOERR_DEL_LIMIT, 0);

    assert_eq!(
        ctx.finish_delete_limit(ctx.skipped_deletes),
        IOERR_DEL_LIMIT
    );
    assert_eq!(count_info(warning), 1, "one warning for the whole walk");
}

/// A general I/O error folded in before segment 2 stops every later
/// deletion, earlier ones stand, and the notice prints once; `--ignore-errors`
/// keeps deleting.
///
/// upstream: generator.c:304-311 - the sticky `io_error & IOERR_GENERAL &&
/// !ignore_errors` test with its static `already_warned`.
#[test]
fn io_error_before_a_segment_stops_later_deletes() {
    logging::init(logging::VerbosityConfig::from_verbose_level(0));
    let _ = logging::drain_events();
    let notice = "IO error encountered -- skipping file deletion";
    for ignore_errors in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        populate(dest);
        let mut ctx = receiver(config(dest, |c| c.deletion.ignore_errors = ignore_errors));
        let mut stats = TransferStats::default();

        for segment in 0..4 {
            if segment == 2 {
                stats.io_error |= IOERR_GENERAL;
            }
            delete_segment(&mut ctx, segment, dest, &mut stats).unwrap();
        }

        assert!(!exists(dest, "a/x"), "a was walked before the error");
        for dir in ["b", "c"] {
            assert_eq!(
                exists(dest, &format!("{dir}/x")),
                !ignore_errors,
                "{dir}/x (ignore_errors={ignore_errors})"
            );
        }
        assert_eq!(
            count_info(notice),
            usize::from(!ignore_errors),
            "notice count (ignore_errors={ignore_errors})"
        );
    }
}

/// A parent that is not a content directory (an implied `--relative` parent)
/// is never scanned, and neither is one whose creation failed.
///
/// upstream: generator.c:2791-2800 - `FLAG_MISSING_DIR` skips the directory,
/// and without `FLAG_CONTENT_DIR` it only gets change_local_filter_dir().
#[test]
fn non_content_and_failed_parents_are_not_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |_| {}));
    ctx.dir_flist.revoke_content_dir(Path::new("a"));
    let mut failed = FailedDirectories::new();
    failed.mark_failed("b");
    let mut stats = TransferStats::default();

    for segment in 1..4 {
        ctx.delete_in_segment(
            segment,
            dest,
            #[cfg(unix)]
            None,
            &failed,
            &mut TestDeletionWriter,
            &mut stats,
        )
        .unwrap();
    }

    assert!(exists(dest, "a/x"), "a is not a content directory");
    assert!(exists(dest, "b/x"), "b failed to be created");
    assert!(!exists(dest, "c/x"), "c is scanned");
}

/// A segment that is not resident - reclaimed, or never received - has no
/// names to build a keep-set from, so the per-directory delete refuses rather
/// than deleting against nothing.
#[test]
fn delete_in_a_reclaimed_or_missing_segment_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |_| {}));
    ctx.first_segment_idx = 2;
    let mut stats = TransferStats::default();

    for segment in [1, 4] {
        assert!(
            delete_segment(&mut ctx, segment, dest, &mut stats).is_err(),
            "segment {segment} is not resident"
        );
    }
    for dir in DIRS {
        assert!(exists(dest, &format!("{dir}/x")), "nothing is deleted");
    }
    assert!(exists(dest, "x"));
}

/// A whole-list sweep after a segment was reclaimed would build its keep-set
/// without that segment's names, so it fails the transfer in every build
/// instead of deleting. The `--delete-delay` execution, which replays victims
/// already decided, is not a sweep and still runs.
#[test]
fn whole_list_sweep_after_reclaim_fails_and_deletes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |_| {}));
    ctx.flist_eof = true;
    ctx.first_segment_idx = 1;
    let mut stats = TransferStats::default();

    let result = ctx.run_receiver_delete_pass(
        DeletePassPhase::Early,
        dest,
        #[cfg(unix)]
        None,
        &mut TestDeletionWriter,
        &mut stats,
    );

    assert!(result.is_err(), "an incomplete keep-set must not be swept");
    assert!(exists(dest, "x"));
    for dir in DIRS {
        assert!(exists(dest, &format!("{dir}/x")), "nothing is deleted");
    }

    let mut delay = receiver(config(dest, |c| c.deletion.late_delete = true));
    delay.flist_eof = true;
    delay.first_segment_idx = 1;
    delay
        .run_receiver_delete_pass(
            DeletePassPhase::Late,
            dest,
            #[cfg(unix)]
            None,
            &mut TestDeletionWriter,
            &mut stats,
        )
        .expect("the delay execution needs no file list");
}

/// A per-directory `.rsync-filter` that an earlier segment already landed in
/// the root protects a matching entry of a later segment's directory; with the
/// merge file absent the same entry is deleted (the opposed control).
///
/// upstream: generator.c:308 delete_in_dir() -> change_local_filter_dir()
/// reloads the destination merge files as each sub-list is walked, so a merge
/// file the walk has already transferred governs every later directory.
#[test]
fn rsync_filter_landed_by_an_earlier_segment_protects_a_later_child() {
    for landed in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        populate(dest);
        std::fs::write(dest.join("a/old.bak"), b"bak").unwrap();
        if landed {
            std::fs::write(dest.join(".rsync-filter"), b"- *.bak\n").unwrap();
        }
        let mut ctx = receiver(config(dest, |_| {}));
        ctx.file_list
            .insert(4, FileEntry::new_file(".rsync-filter".into(), 8, 0o644));
        ctx.ndx_segments = vec![(0, 1), (5, 7), (6, 9), (7, 11)];
        let mut chain = ::filters::FilterChain::empty();
        chain.add_merge_config(::filters::DirMergeConfig::new(".rsync-filter"));
        ctx.set_filter_chain(chain);
        let mut stats = TransferStats::default();

        delete_segment(&mut ctx, 1, dest, &mut stats).unwrap();

        assert_eq!(exists(dest, "a/old.bak"), landed, "landed={landed}");
        assert!(!exists(dest, "a/x"), "an unprotected extra is deleted");
        assert!(exists(dest, "a/k"));
    }
}

/// `--delete-delay` records each segment's victims without unlinking them;
/// the late site unlinks them all after the walk.
///
/// upstream: generator.c:352-353 remember_delete() inside delete_in_dir(),
/// generator.c:2899-2900 do_delayed_deletions() after generate_files()'s walk.
#[test]
fn delete_delay_collects_per_segment_and_unlinks_late() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path();
    populate(dest);
    let mut ctx = receiver(config(dest, |c| c.deletion.late_delete = true));
    ctx.flist_eof = true;
    let mut stats = TransferStats::default();

    for segment in 0..4 {
        delete_segment(&mut ctx, segment, dest, &mut stats).unwrap();
    }
    assert!(exists(dest, "x"), "nothing is unlinked during the walk");
    for dir in DIRS {
        assert!(exists(dest, &format!("{dir}/x")));
    }
    assert_eq!(ctx.delayed_delete_victims.len(), 4);

    ctx.run_receiver_delete_pass(
        DeletePassPhase::Late,
        dest,
        #[cfg(unix)]
        None,
        &mut TestDeletionWriter,
        &mut stats,
    )
    .unwrap();
    assert!(!exists(dest, "x"));
    for dir in DIRS {
        assert!(!exists(dest, &format!("{dir}/x")));
        assert!(exists(dest, &format!("{dir}/k")));
    }
    assert_eq!(ctx.effective_del_stats().files, 4);
}

/// Without a delete mode, or in `--list-only`, the per-directory delete does
/// nothing.
#[test]
fn per_directory_delete_needs_a_during_walk_mode() {
    type Tweak = fn(&mut ServerConfig);
    let cases: [(&str, Tweak); 3] = [
        ("no --delete", |c| c.flags.delete = false),
        ("--list-only", |c| c.flags.list_only = true),
        ("--delete-after", |c| {
            c.deletion.delete_after = true;
            c.deletion.late_delete = true;
        }),
    ];
    for (name, tweak) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        populate(dest);
        let mut ctx = receiver(config(dest, tweak));
        let mut stats = TransferStats::default();

        for segment in 0..4 {
            delete_segment(&mut ctx, segment, dest, &mut stats).unwrap();
        }
        assert!(exists(dest, "x"), "{name}: nothing is deleted");
        assert!(exists(dest, "a/x"), "{name}: nothing is deleted");
    }
}
