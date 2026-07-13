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
/// way upstream does: only `--delete-after` (`delete_after`) defers the delete
/// *decision* to after the transfer (generator.c:2427-2428); `--delete-before`,
/// `--delete-during`, AND `--delete-delay` all decide early. `--delete-delay`
/// belongs with the early group because upstream makes its decision during the
/// walk (generator.c:2315) and defers only the unlink - verified vs upstream
/// 3.4.4 over SSH, where delay DELETES a per-dir-merge-protected entry exactly
/// as during/before do, while after PROTECTS it. A regression that deferred
/// delay would over-protect (keep files upstream deletes); one that failed to
/// defer after would reintroduce the #280 data loss. The sweep runs exactly
/// once when `--delete` is set (never both, never neither).
#[test]
fn delete_pass_timing_predicates_partition_the_four_modes() {
    let handshake = test_handshake();

    // --delete-before / --delete-during / --delete-delay: delete on, but the
    // decision is NOT deferred (`delete_after` off). late_delete may be set for
    // delay (it governs only goodbye del-stats), which must not affect routing.
    for (label, late_delete) in [("before/during", false), ("delay", true)] {
        let mut early = test_config();
        early.flags.delete = true;
        early.deletion.delete_after = false;
        early.deletion.late_delete = late_delete;
        let ctx = ReceiverContext::new_for_test(&handshake, early);
        assert!(ctx.delete_pass_is_early(), "{label} must sweep early");
        assert!(!ctx.delete_pass_is_late(), "{label} must not sweep late");
    }

    // --delete-after: delete on, decision deferred.
    let mut late = test_config();
    late.flags.delete = true;
    late.deletion.late_delete = true;
    late.deletion.delete_after = true;
    let ctx = ReceiverContext::new_for_test(&handshake, late);
    assert!(!ctx.delete_pass_is_early(), "after must not sweep early");
    assert!(ctx.delete_pass_is_late(), "after must sweep late");

    // No --delete: neither site fires, regardless of the deferral bits (which a
    // stale config could still carry). The sweep must never run unrequested.
    for delete_after in [false, true] {
        let mut off = test_config();
        off.flags.delete = false;
        off.deletion.delete_after = delete_after;
        let ctx = ReceiverContext::new_for_test(&handshake, off);
        assert!(
            !ctx.delete_pass_is_early() && !ctx.delete_pass_is_late(),
            "no --delete => no sweep (delete_after={delete_after})"
        );
    }
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
