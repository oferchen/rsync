// A source directory whose contents cannot be enumerated (mode 0300: write and
// search, no read) is still a directory upstream sends: `send_file_entry()` and
// `send_acl()` run before `opendir()` (flist.c:1847-1858), and a failed
// `opendir()` only aborts the descent (flist.c:2129-2140). The receiver
// therefore still creates the directory and applies its permissions and ACLs.
// These tests pin that the enumeration failure stays enumeration-only.
//
// They must run as a non-root user: root holds CAP_DAC_READ_SEARCH, the mode
// check never fires, and the fixture becomes structurally unable to fail.

#[cfg(unix)]
fn running_as_root() -> bool {
    // `rustix`'s raw getuid is not intercepted by fakeroot, so a fakeroot run
    // reports the real uid and correctly takes the non-root path.
    rustix::process::getuid().is_root()
}

/// Builds `<root>/src/dropbox/inner.txt` plus `<root>/src/top.txt` and a
/// pre-existing `<root>/dest/dropbox`, returning `(source, dest)`.
#[cfg(unix)]
fn build_unreadable_dir_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("src");
    let dest = root.join("dest");
    fs::create_dir_all(source.join("dropbox")).expect("create source dropbox");
    fs::write(source.join("dropbox/inner.txt"), b"inner").expect("write inner");
    fs::write(source.join("top.txt"), b"top\n").expect("write top");
    fs::create_dir_all(dest.join("dropbox")).expect("create dest dropbox");
    (source, dest)
}

#[cfg(unix)]
fn copy_tree_with(
    source: &Path,
    dest: &Path,
    options: LocalCopyOptions,
) -> Result<LocalCopySummary, LocalCopyError> {
    let mut source_operand = source.to_path_buf().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.to_path_buf().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
}

/// Re-opens every 0300 directory in the fixture so `TempDir` can remove it.
#[cfg(unix)]
fn restore_fixture_modes(paths: &[PathBuf]) {
    use std::os::unix::fs::PermissionsExt;
    for path in paths {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_source_directory_still_applies_its_own_metadata() {
    // The defect: the recursive frame propagated the readdir error immediately,
    // so `ensure_directory` and `apply_final_directory_metadata` never ran and
    // the directory's own mode (and, with --acls, its ACL) was never applied.
    // Upstream's entry was already sent before the opendir, so the receiver
    // does apply it. Exit stays 23 because the enumeration itself failed.
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let (source, dest) = build_unreadable_dir_fixture(temp.path());
    fs::set_permissions(source.join("dropbox"), fs::Permissions::from_mode(0o300))
        .expect("make source dropbox unreadable");
    // A mode the transfer must overwrite, so a passing assertion cannot be the
    // pre-existing state.
    fs::set_permissions(dest.join("dropbox"), fs::Permissions::from_mode(0o755))
        .expect("seed dest dropbox mode");

    let result = copy_tree_with(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .permissions(true),
    );

    let dest_mode = fs::metadata(dest.join("dropbox"))
        .expect("dest dropbox exists")
        .permissions()
        .mode()
        & 0o777;
    restore_fixture_modes(&[source.join("dropbox"), dest.join("dropbox")]);

    assert_eq!(
        dest_mode, 0o300,
        "the unreadable source directory's own mode must reach the destination"
    );
    // Its readable sibling still transfers: the failure is scoped to the frame
    // that could not be enumerated.
    assert_eq!(
        fs::read(dest.join("top.txt")).expect("top.txt copied"),
        b"top\n"
    );

    let error = result.expect_err("unreadable source directory must report an I/O error");
    assert_eq!(
        error.exit_code(),
        23,
        "an unreadable directory is RERR_PARTIAL (upstream IOERR_GENERAL), got {error}"
    );
}

#[cfg(unix)]
#[test]
fn readable_source_directory_still_transfers_its_contents() {
    // Non-vacuity companion for the tests above: with the identical fixture and
    // a *readable* source directory the walk is unchanged - contents transfer
    // and the run succeeds. Without this, a fix that simply stopped descending
    // into every directory would satisfy the assertions above.
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let (source, dest) = build_unreadable_dir_fixture(temp.path());
    fs::set_permissions(source.join("dropbox"), fs::Permissions::from_mode(0o700))
        .expect("keep source dropbox readable");

    let summary = copy_tree_with(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .permissions(true),
    )
    .expect("readable source directory transfers cleanly");

    assert_eq!(
        fs::read(dest.join("dropbox/inner.txt")).expect("inner.txt copied"),
        b"inner"
    );
    assert!(summary.files_copied() >= 2, "both files must transfer");
    assert_eq!(
        fs::metadata(dest.join("dropbox"))
            .expect("dest dropbox exists")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn prune_empty_dirs_keeps_a_directory_whose_contents_were_unreadable() {
    // `--prune-empty-dirs` decides on the entries it saw. A directory that
    // could not be enumerated produced no entries because it could not be read,
    // not because it is empty, so pruning it would delete a directory on the
    // strength of a failed read - and would also return before the metadata
    // apply, silently defeating the fix. The same failure must suppress the
    // delete pass (upstream: generator.c:304-311 delete_in_dir()).
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let (source, dest) = build_unreadable_dir_fixture(temp.path());
    fs::set_permissions(source.join("dropbox"), fs::Permissions::from_mode(0o300))
        .expect("make source dropbox unreadable");
    // The destination directory must be *created* by this run, otherwise
    // `handle_empty_directory_pruning` would decline to remove it anyway and
    // the assertion below could not fail.
    fs::remove_dir_all(dest.join("dropbox")).expect("drop pre-existing dest dropbox");
    // The extraneous entry has to live in a directory swept *after* the
    // unreadable one: `--delete-during` sweeps a directory before descending
    // into its children, so the transfer root's own sweep runs before any
    // child records an error. "zsub" sorts after "dropbox".
    fs::create_dir_all(source.join("zsub")).expect("create source zsub");
    fs::write(source.join("zsub/keep.txt"), b"keep").expect("write keep");
    fs::create_dir_all(dest.join("zsub")).expect("create dest zsub");
    fs::write(dest.join("zsub/keep.txt"), b"keep").expect("write dest keep");
    fs::write(dest.join("zsub/extra.txt"), b"should survive").expect("write extra");

    let result = copy_tree_with(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .permissions(true)
            .prune_empty_dirs(true)
            .delete(true),
    );

    let dropbox_present = dest.join("dropbox").is_dir();
    restore_fixture_modes(&[source.join("dropbox"), dest.join("dropbox")]);

    assert!(
        dropbox_present,
        "--prune-empty-dirs must not remove a directory that could not be read"
    );
    assert!(
        dest.join("zsub/extra.txt").exists(),
        "the enumeration failure must suppress the later delete pass (generator.c:304)"
    );
    assert_eq!(
        result
            .expect_err("unreadable source directory must report an I/O error")
            .exit_code(),
        23
    );
}

#[cfg(all(target_os = "linux", feature = "acl"))]
#[test]
fn acls_reach_an_unreadable_source_directory_and_revoke_the_stale_entry() {
    // testsuite/acls-unpinnable_test.py: a 0300 source directory's ACL must
    // still be applied to the destination, and a different (stale) grant
    // already on the destination must be revoked. Both were lost because the
    // frame died at the readdir, never reaching sync_acls().
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }

    const NEW_UID: u32 = 60002;
    const STALE_UID: u32 = 60009;

    let temp = tempdir().expect("tempdir");
    let (source, dest) = build_unreadable_dir_fixture(temp.path());

    let source_dropbox = source.join("dropbox");
    let dest_dropbox = dest.join("dropbox");
    set_acl_from_text(
        &source_dropbox,
        &format!("user::rwx\ngroup::---\nother::---\nuser:{NEW_UID}:rwx\nmask::rwx\n"),
        acl_sys::ACL_TYPE_ACCESS,
    );
    set_acl_from_text(
        &dest_dropbox,
        &format!("user::rwx\ngroup::---\nother::---\nuser:{STALE_UID}:rwx\nmask::rwx\n"),
        acl_sys::ACL_TYPE_ACCESS,
    );
    // The filesystem may have ACLs disabled; skip rather than assert nonsense.
    let seeded = acl_to_text(&dest_dropbox, acl_sys::ACL_TYPE_ACCESS).unwrap_or_default();
    if !seeded.contains(&format!("user:{STALE_UID}:")) {
        return;
    }
    fs::set_permissions(&source_dropbox, fs::Permissions::from_mode(0o300))
        .expect("make source dropbox unreadable");
    fs::set_permissions(&dest_dropbox, fs::Permissions::from_mode(0o300))
        .expect("make dest dropbox unreadable");

    let result = copy_tree_with(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .permissions(true)
            .acls(true),
    );

    let applied = acl_to_text(&dest_dropbox, acl_sys::ACL_TYPE_ACCESS).unwrap_or_default();
    restore_fixture_modes(&[source_dropbox, dest_dropbox]);

    assert!(
        applied.contains(&format!("user:{NEW_UID}:")),
        "--acls did not apply the source ACL to the unreadable (0300) destination \
         directory; getfacl:\n{applied}"
    );
    assert!(
        !applied.contains(&format!("user:{STALE_UID}:")),
        "--acls left the stale destination ACL entry in place on the unreadable \
         (0300) directory - a revocation did not propagate; getfacl:\n{applied}"
    );
    assert_eq!(
        result
            .expect_err("unreadable source directory must report an I/O error")
            .exit_code(),
        23
    );
}
