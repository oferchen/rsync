//! Late (`--delete-after` / `--delete-delay`) vs early (`--delete-before` /
//! `--delete-during`) delete-pass scheduling on the receiver.
//!
//! WHY this matters (the #280 data-loss bug): upstream defers the delete sweep
//! for `--delete-after` / `--delete-delay` until after every file has been
//! transferred (generator.c:2425-2428), because the sweep reloads each
//! destination directory's per-directory `.rsync-filter` merge file at delete
//! time (exclude.c:875 `change_local_filter_dir`). A merge-file protect rule
//! (e.g. `- *.bak`) can only shield a matching destination entry once that
//! `.rsync-filter` is present in the destination - which it is not until the
//! transfer has run. Running the sweep before the transfer (as oc did for all
//! four modes) therefore deleted files upstream protects.
//!
//! These tests pin the two halves of the fix:
//!   1. the timing predicates that route the sweep early vs late, and
//!   2. the load-bearing invariant that protection depends on the destination
//!      `.rsync-filter` being present at delete time (proving deferral is not
//!      cosmetic).

use std::ffi::OsString;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{TestDeletionWriter, test_config, test_handshake};

/// The early/late routing predicates must partition the four delete modes the
/// way upstream does:
///
/// - `--delete-before` / `--delete-during`: EARLY only (immediate pre-loop
///   sweep, generator.c:2280 / 2315).
/// - `--delete-after`: LATE only (`do_delete_pass()` after the transfer,
///   generator.c:2427).
/// - `--delete-delay`: BOTH sites. Upstream decides the victim set during the
///   walk (generator.c:2315 `remember_delete`) but unlinks them only in
///   `do_delayed_deletions()` after the whole transfer (generator.c:2419), so
///   oc collects at the early site and executes at the late site. Verified vs
///   upstream 3.4.4 over SSH: delay DELETES a per-dir-merge-protected entry
///   (its decision matches during/before), yet the unlink is deferred to the
///   end (a mid-transfer abort leaves the stale file in place).
///
/// A regression that dropped delay from the early site would decide too late
/// (over-protecting merge-shielded entries like `--delete-after`); one that
/// dropped it from the late site would unlink during the transfer (losing the
/// crash-safety upstream guarantees). The sweep never runs when `--delete` is
/// off, regardless of stale deferral bits.
#[test]
fn delete_pass_timing_predicates_partition_the_four_modes() {
    let handshake = test_handshake();

    // --delete-before / --delete-during: EARLY only.
    let mut early = test_config();
    early.flags.delete = true;
    early.deletion.delete_after = false;
    early.deletion.late_delete = false;
    let ctx = ReceiverContext::new_for_test(&handshake, early);
    assert!(ctx.delete_pass_is_early(), "before/during must run early");
    assert!(
        !ctx.delete_pass_is_late(),
        "before/during must not run late"
    );

    // --delete-delay: BOTH sites (collect early, execute late).
    let mut delay = test_config();
    delay.flags.delete = true;
    delay.deletion.delete_after = false;
    delay.deletion.late_delete = true;
    let ctx = ReceiverContext::new_for_test(&handshake, delay);
    assert!(ctx.delete_pass_is_early(), "delay must collect early");
    assert!(ctx.delete_pass_is_late(), "delay must execute late");

    // --delete-after: LATE only (decision deferred).
    let mut late = test_config();
    late.flags.delete = true;
    late.deletion.late_delete = true;
    late.deletion.delete_after = true;
    let ctx = ReceiverContext::new_for_test(&handshake, late);
    assert!(!ctx.delete_pass_is_early(), "after must not run early");
    assert!(ctx.delete_pass_is_late(), "after must run late");

    // No --delete: neither site fires, regardless of the deferral bits (which a
    // stale config could still carry). The sweep must never run unrequested.
    for delete_after in [false, true] {
        let mut off = test_config();
        off.flags.delete = false;
        off.deletion.delete_after = delete_after;
        off.deletion.late_delete = delete_after;
        let ctx = ReceiverContext::new_for_test(&handshake, off);
        assert!(
            !ctx.delete_pass_is_early() && !ctx.delete_pass_is_late(),
            "no --delete => no sweep (delete_after={delete_after})"
        );
    }
}

/// The load-bearing `--delete-during` vs `--delete-delay` timing distinction:
/// during unlinks an extraneous file as its directory is processed, while delay
/// only *records* the victim during the walk and unlinks it after the whole
/// transfer completes. This is upstream's crash-safety guarantee for delay
/// (generator.c:345 `remember_delete` vs generator.c:2419
/// `do_delayed_deletions`): if the transfer aborts mid-way, a delay run has not
/// deleted anything yet, whereas a during run already has.
///
/// The test proves the distinction directly: `collect_delayed_deletions`
/// (delay's early phase) must leave the stale file ON DISK - standing in for the
/// mid-transfer state - while returning it as a pending victim; only
/// `execute_delayed_deletions` (delay's late phase) removes it. An immediate
/// sweep (during/before) removes the very same file in a single call. Both modes
/// converge on the identical final set: stale gone, listed file kept.
#[test]
fn delete_delay_defers_unlink_until_execute_phase() {
    let handshake = test_handshake();

    // Shared destination layout builder: one extraneous file plus one listed
    // file that must always survive. Returns a receiver whose file list carries
    // only the survivor, making `stale.txt` an extraneous deletion candidate.
    let build = |dest: &std::path::Path| {
        std::fs::write(dest.join("stale.txt"), b"extraneous").unwrap();
        std::fs::write(dest.join("keep.txt"), b"listed").unwrap();
        let mut config = test_config();
        config.flags.delete = true;
        config.deletion.delete_after = false;
        config.deletion.late_delete = true; // --delete-delay
        config.args = vec![OsString::from(dest.to_str().unwrap())];
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);
        ctx.file_list
            .push(FileEntry::new_directory(".".into(), 0o755));
        ctx.file_list
            .push(FileEntry::new_file("keep.txt".into(), 6, 0o644));
        ctx
    };

    // --delete-delay: collect must NOT unlink (mid-transfer crash-safety).
    let delay_dir = tempfile::TempDir::new().unwrap();
    let dest = delay_dir.path();
    let ctx = build(dest);
    let mut writer = TestDeletionWriter;
    let (victims, _io) = ctx
        .collect_delayed_deletions(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();
    assert!(
        dest.join("stale.txt").exists(),
        "delay must NOT unlink during collection - the file survives a mid-transfer abort",
    );
    assert!(!victims.is_empty(), "delay must record the pending victim");

    // The late phase executes the recorded victims: now the file is gone.
    let (stats, _io) = ctx
        .execute_delayed_deletions(
            dest,
            #[cfg(unix)]
            None,
            &victims,
            &mut writer,
        )
        .unwrap();
    assert!(
        !dest.join("stale.txt").exists(),
        "delay must unlink the recorded victim at the execute (late) phase",
    );
    assert_eq!(stats.files, 1, "exactly one extraneous file deleted");
    assert!(
        dest.join("keep.txt").exists(),
        "listed file must survive delay"
    );

    // --delete-during / --delete-before: an immediate sweep removes the same
    // stale file in one call, and the surviving set is identical.
    let during_dir = tempfile::TempDir::new().unwrap();
    let dest2 = during_dir.path();
    let ctx2 = build(dest2);
    let (during_stats, _, _) = ctx2
        .delete_extraneous_files(
            dest2,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();
    assert!(
        !dest2.join("stale.txt").exists(),
        "during/before unlinks the stale file immediately",
    );
    assert_eq!(
        during_stats.files, stats.files,
        "during and delay must delete the identical final set",
    );
    assert!(
        dest2.join("keep.txt").exists(),
        "listed file must survive during"
    );
}

/// Builds the destination tree and receiver used by both invariant tests: a
/// nested `sub/` directory, an extraneous `normal.bak` at the root and an
/// extraneous `sub/x.bak`, plus a `source.txt` that is present in the file
/// list. The file list carries `.`, `sub`, `source.txt`, and `.rsync-filter`
/// (the merge file itself is transferred, so it is a kept entry), but NOT the
/// `.bak` files, which are therefore extraneous deletion candidates. The
/// receiver's deletion chain has a `.rsync-filter` per-directory merge config
/// registered, exactly as `-F` / `--filter=': /.rsync-filter'` would install.
fn build_receiver_with_perdir_merge(dest: &std::path::Path) -> ReceiverContext {
    std::fs::create_dir(dest.join("sub")).unwrap();
    std::fs::write(dest.join("normal.bak"), b"extraneous root bak").unwrap();
    std::fs::write(dest.join("sub").join("x.bak"), b"extraneous nested bak").unwrap();
    std::fs::write(dest.join("source.txt"), b"from sender").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    // Represents --delete-after: the deferred delete pass runs after the
    // transfer, when the destination `.rsync-filter` is present.
    config.deletion.late_delete = true;
    config.deletion.delete_after = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("sub".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("source.txt".into(), 11, 0o644));
    ctx.file_list
        .push(FileEntry::new_file(".rsync-filter".into(), 8, 0o644));

    // Register the dest-side per-directory `.rsync-filter` merge config on the
    // receiver's wire-populated `filter_chain`, exactly as a server receiver does
    // after parsing a `-F` (`dir-merge /.rsync-filter`) rule off the wire
    // (setup/wire_filters.rs -> parse_wire_filters_for_receiver). The delete pass
    // (deletion.rs) consults this chain and reloads each destination directory's
    // `.rsync-filter` at delete time (upstream exclude.c:759 push_local_filters).
    // The chain carries no global rules, so protection can only come from a
    // `.rsync-filter` read off the disk - which is the whole point of deferral.
    let mut chain = ::filters::FilterChain::empty();
    chain.add_merge_config(::filters::DirMergeConfig::new(".rsync-filter"));
    ctx.set_filter_chain(chain);
    ctx
}

/// LATE case: when the destination `.rsync-filter` (`- *.bak`) is already on
/// disk at delete time - the state after the transfer has landed it - the
/// deferred sweep reloads it per directory and the protect rule shields both
/// the root `normal.bak` and the inherited nested `sub/x.bak`. This is the
/// behaviour upstream produces for `--delete-after` / `--delete-delay`, and the
/// on-disk survival is exactly what oc failed to preserve before #280.
#[test]
fn late_delete_pass_with_dest_rsync_filter_protects_bak() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();
    let ctx = build_receiver_with_perdir_merge(dest);

    // The `.rsync-filter` has landed (transferred). `- *.bak` excludes, and an
    // excluded entry is absent from the delete candidate list, hence protected.
    std::fs::write(dest.join(".rsync-filter"), b"- *.bak\n").unwrap();

    let mut writer = TestDeletionWriter;
    let (_stats, _, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    assert!(
        dest.join("normal.bak").exists(),
        "root normal.bak must be protected by the dest .rsync-filter '- *.bak'"
    );
    assert!(
        dest.join("sub").join("x.bak").exists(),
        "nested sub/x.bak must be protected by the inherited '- *.bak' rule"
    );
    assert!(dest.join("source.txt").exists(), "listed file must survive");
}

/// TIMING CONTRAST: the very same sweep, run while the destination
/// `.rsync-filter` is NOT yet present (the state at an early, pre-transfer
/// sweep), deletes the `.bak` files - there is no on-disk merge file to load,
/// so nothing protects them. This is why the fix defers the sweep for
/// after/delay: protection is a function of the `.rsync-filter` being on disk,
/// which only holds once the transfer has run. If deferral regressed to an
/// early sweep, the after/delay data loss would return - this test would then
/// pass for the wrong reason, so it is paired with the LATE test above.
#[test]
fn delete_pass_without_dest_rsync_filter_deletes_bak() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();
    let ctx = build_receiver_with_perdir_merge(dest);

    // No `.rsync-filter` written: mirror an early sweep before the merge file
    // has been transferred into the destination.
    let mut writer = TestDeletionWriter;
    let (_stats, _, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    assert!(
        !dest.join("normal.bak").exists(),
        "without a dest .rsync-filter nothing protects normal.bak"
    );
    assert!(
        !dest.join("sub").join("x.bak").exists(),
        "without a dest .rsync-filter nothing protects sub/x.bak"
    );
    assert!(dest.join("source.txt").exists(), "listed file must survive");
}
