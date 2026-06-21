//! Receiver-side filter chain: protect rules gate `delete_extraneous_files`
//! and an empty chain is a no-op. Verifies the set/get accessor pair on
//! `ReceiverContext`.

use std::ffi::OsString;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{TestDeletionWriter, test_config, test_handshake};

#[test]
fn receiver_filter_chain_protects_from_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Create files at destination (extra files that should be deleted)
    std::fs::write(dest.join("normal.txt"), b"delete me").unwrap();
    std::fs::write(dest.join("protected.conf"), b"keep me").unwrap();
    std::fs::write(dest.join("source.txt"), b"from sender").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // File list includes "." and "source.txt" - anything else at dest is extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("source.txt".into(), 11, 0o644));

    // Set up filter chain with protect rule for *.conf
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::protect("*.conf")]).unwrap();
    ctx.set_filter_chain(::filters::FilterChain::new(global));

    let mut writer = TestDeletionWriter;
    let (stats, _, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    // normal.txt should be deleted (not in file list, not protected)
    assert!(
        !dest.join("normal.txt").exists(),
        "normal.txt should be deleted"
    );

    // protected.conf should survive due to protect rule
    assert!(
        dest.join("protected.conf").exists(),
        "protected.conf should be protected from deletion"
    );

    // source.txt should survive (it's in the file list)
    assert!(dest.join("source.txt").exists());

    assert!(stats.files >= 1); // At least normal.txt was deleted
}

#[test]
fn receiver_filter_chain_empty_allows_all_deletions() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("file1.txt"), b"data1").unwrap();
    std::fs::write(dest.join("file2.log"), b"data2").unwrap();
    std::fs::write(dest.join("keep.txt"), b"keep").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // File list has "." and "keep.txt" - file1/file2 are extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 4, 0o644));

    // Empty filter chain - all deletions should proceed
    let mut writer = TestDeletionWriter;
    let (stats, _, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    assert!(!dest.join("file1.txt").exists());
    assert!(!dest.join("file2.log").exists());
    assert!(dest.join("keep.txt").exists());
    assert_eq!(stats.files, 2);
}

/// Regression test for the upstream-testsuite `daemon-delete-stats` failure.
///
/// The test's daemon config carries a global `exclude = ? foobar.baz` rule.
/// Single-character glob `?` previously caused deletion to skip every
/// top-level extraneous file because `delete_extraneous_files()` queried the
/// filter chain with `"./<name>"`. The descendant matcher `?/**` derived
/// from the bare `?` pattern then treated `.` as a single-character parent
/// directory and incorrectly excluded the candidate, leaving
/// `delete.txt` in place. The fix is to strip the implicit `.` prefix
/// before consulting the filter chain at the deletion root.
#[test]
fn single_char_wildcard_exclude_does_not_block_top_level_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("delete.txt"), b"delete\n").unwrap();
    std::fs::write(dest.join("keep.txt"), b"keep\n").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Source advertises only `.` and `keep.txt`; `delete.txt` is extraneous.
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 5, 0o644));

    // Daemon-level `exclude = ? foobar.baz` reproduction.
    let global = ::filters::FilterSet::from_rules([
        ::filters::FilterRule::exclude("?"),
        ::filters::FilterRule::exclude("foobar.baz"),
    ])
    .unwrap();
    ctx.set_filter_chain(::filters::FilterChain::new(global));

    let mut writer = TestDeletionWriter;
    let (stats, _, _) = ctx
        .delete_extraneous_files(
            dest,
            #[cfg(unix)]
            None,
            &mut writer,
        )
        .unwrap();

    assert!(
        !dest.join("delete.txt").exists(),
        "delete.txt must be deleted despite the `?` exclude rule"
    );
    assert!(dest.join("keep.txt").exists(), "keep.txt must survive");
    assert_eq!(stats.files, 1);
}

/// Regression: a per-directory merge file must be honored during the
/// `--delete` pass, mirroring upstream `delete_in_dir` which calls
/// `change_local_filter_dir` to reload that directory's merge filters
/// before scanning it for extraneous entries.
///
/// Before the fix, the receiver cloned a shared `FilterChain` carrying
/// only the global rules and never called `enter_directory`, so a
/// `.rsync-filter` placed inside a subdirectory was ignored. An
/// extraneous file the merge file protected (`- *.deep`) was therefore
/// wrongly deleted, while the upstream tests `exclude` / `exclude-lsh`
/// expect it to survive. This pins the per-worker per-directory reload:
/// the protected file must survive and an unprotected sibling must still
/// be deleted.
///
/// upstream: generator.c:308 change_local_filter_dir in delete_in_dir.
#[test]
fn per_dir_merge_filter_protects_during_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    let subdir = dest.join("sub");
    std::fs::create_dir(&subdir).unwrap();

    // Per-directory merge file protecting `*.deep` from deletion.
    std::fs::write(subdir.join(".rsync-filter"), b"- *.deep\n").unwrap();

    // keep.txt is in the source flist; protected.deep and extraneous.txt
    // are both extraneous (absent from the flist).
    std::fs::write(subdir.join("keep.txt"), b"keep").unwrap();
    std::fs::write(subdir.join("protected.deep"), b"protected").unwrap();
    std::fs::write(subdir.join("extraneous.txt"), b"extraneous").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("sub".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("sub/keep.txt".into(), 4, 0o644));
    // The merge file is in the flist so it is not itself a deletion
    // candidate; the test isolates the `*.deep` protection.
    ctx.file_list
        .push(FileEntry::new_file("sub/.rsync-filter".into(), 8, 0o644));

    // Global chain carries no path rules; the protection lives entirely in
    // the per-directory `.rsync-filter`, registered as a dir-merge config.
    let mut chain = ::filters::FilterChain::empty();
    chain.add_merge_config(::filters::DirMergeConfig::new(".rsync-filter"));
    ctx.set_filter_chain(chain);

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
        subdir.join("protected.deep").exists(),
        "protected.deep must survive: the per-dir .rsync-filter excludes it from deletion",
    );
    assert!(
        !subdir.join("extraneous.txt").exists(),
        "extraneous.txt must be deleted: no merge rule protects it",
    );
    assert!(
        subdir.join("keep.txt").exists(),
        "keep.txt must survive: it is in the source file list",
    );
}

/// Regression: an inheriting per-directory merge rule defined in an
/// ancestor must protect a deletion candidate several levels below it,
/// the way upstream `exclude-lsh` relies on `bar/down/to/bar/.filt2`
/// (`- *.deep`) protecting `bar/down/to/bar/baz/file5.deep`.
///
/// The receiver re-enters every ancestor of the scan target before
/// listing it, so the inheriting scope pushed at the ancestor's depth
/// must still apply at the deeper scan depth. The per-entry deletion
/// check passes a transfer-root-relative path (`top/mid/protected.deep`),
/// matching the path convention the generator uses for `allows`, so the
/// inherited rule fires. Pins that the deeper candidate is protected
/// while a sibling the rule does not name is still deleted.
///
/// upstream: exclude-lsh.test:75-78, generator.c:308 change_local_filter_dir.
#[test]
fn inherited_per_dir_merge_protects_deeper_deletion_candidate() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // dest/top/.filt2 = `- *.deep` (inheriting); the candidate lives two
    // levels below in dest/top/mid/leaf.
    let top = dest.join("top");
    let mid = top.join("mid");
    std::fs::create_dir_all(&mid).unwrap();
    std::fs::write(top.join(".filt2"), b"- *.deep\n").unwrap();

    std::fs::write(mid.join("keep.txt"), b"keep").unwrap();
    std::fs::write(mid.join("protected.deep"), b"deep").unwrap();
    std::fs::write(mid.join("extraneous.txt"), b"extra").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("top".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("top/.filt2".into(), 8, 0o644));
    ctx.file_list
        .push(FileEntry::new_directory("top/mid".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("top/mid/keep.txt".into(), 4, 0o644));

    let mut chain = ::filters::FilterChain::empty();
    chain.add_merge_config(::filters::DirMergeConfig::new(".filt2"));
    ctx.set_filter_chain(chain);

    let mut writer = TestDeletionWriter;
    ctx.delete_extraneous_files(
        dest,
        #[cfg(unix)]
        None,
        &mut writer,
    )
    .unwrap();

    assert!(
        mid.join("protected.deep").exists(),
        "protected.deep must survive: the ancestor `top/.filt2 - *.deep` inherits down to `top/mid`",
    );
    assert!(
        !mid.join("extraneous.txt").exists(),
        "extraneous.txt must be deleted: no merge rule protects it",
    );
    assert!(
        mid.join("keep.txt").exists(),
        "keep.txt must survive: it is in the source file list",
    );
}

/// Regression: a non-inheriting `:C`/.cvsignore scope must protect a
/// candidate in the directory it was loaded in, but must NOT leak into a
/// child directory. Mirrors upstream `exclude-lsh` where `mid/.filt`
/// (`:C`) loads `mid/.cvsignore` for `mid` only.
///
/// The receiver pushes the `:C` scope at the scan directory's depth; the
/// depth-gated `scope_applies_here` check (filters chain `scope_applies_here`)
/// keeps it from suppressing deletions one level deeper. Pins both halves
/// so a regression in the per-worker ancestor-walk depth bookkeeping is
/// caught.
///
/// upstream: exclude-lsh.test:80-88, exclude.c FILTRULE_NO_INHERIT.
#[test]
fn non_inheriting_cvsignore_scope_is_depth_local_during_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // dest/.cvsignore protects `ignored` in the root only (non-inheriting).
    let sub = dest.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(dest.join(".cvsignore"), b"ignored\n").unwrap();

    std::fs::write(dest.join("ignored"), b"top").unwrap();
    std::fs::write(sub.join("ignored"), b"deep").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file(".cvsignore".into(), 8, 0o644));
    ctx.file_list
        .push(FileEntry::new_directory("sub".into(), 0o755));

    // `.cvsignore` registered as a non-inheriting CVS-mode dir-merge, the
    // way upstream's `:C` directive expands it.
    let mut chain = ::filters::FilterChain::empty();
    chain.add_merge_config(
        ::filters::DirMergeConfig::new(".cvsignore")
            .with_cvs_mode(true)
            .with_inherit(false),
    );
    ctx.set_filter_chain(chain);

    let mut writer = TestDeletionWriter;
    ctx.delete_extraneous_files(
        dest,
        #[cfg(unix)]
        None,
        &mut writer,
    )
    .unwrap();

    assert!(
        dest.join("ignored").exists(),
        "root `ignored` must survive: dest/.cvsignore protects it at the root depth",
    );
    assert!(
        !sub.join("ignored").exists(),
        "sub/ignored must be deleted: the non-inheriting `.cvsignore` scope does not leak into `sub`",
    );
}

#[test]
fn receiver_set_and_get_filter_chain() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Default filter chain should be empty
    assert!(ctx.filter_chain().is_empty());

    // Set a chain with rules
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::exclude("*.bak")]).unwrap();
    let chain = ::filters::FilterChain::new(global);
    ctx.set_filter_chain(chain);

    assert!(!ctx.filter_chain().is_empty());
}

/// UTS-16.b.7 regression: an ordinary in-module subdir deletion must succeed
/// with no io_error bits set, even when the sandbox is plumbed. Pins the
/// "no over-correction" invariant: the chdir-symlink-race fix in
/// `delete_extraneous_files` must not poison legitimate sweeps with
/// `IOERR_GENERAL`.
///
/// upstream: generator.c:delete_in_dir() - the legitimate path issues a
/// successful `secure_relative_open` and never touches io_error.
#[cfg(unix)]
#[test]
fn delete_ordinary_subdir_succeeds_with_no_io_error() {
    use std::os::unix::ffi::OsStrExt;
    use std::sync::Arc;

    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    // Canonicalize so the sandbox open does not trip on macOS
    // `/tmp -> /private/tmp` under RESOLVE_NO_SYMLINKS.
    let dest = std::fs::canonicalize(temp_dir.path()).unwrap();

    // Build an in-module subdir with an extraneous file that should be deleted.
    let subdir = dest.join("subdir");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("keep.txt"), b"keep").unwrap();
    std::fs::write(subdir.join("extraneous.txt"), b"extraneous").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(
        std::str::from_utf8(dest.as_os_str().as_bytes()).unwrap(),
    )];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Sender's flist: "." + subdir + subdir/keep.txt. extraneous.txt is missing.
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("subdir".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("subdir/keep.txt".into(), 4, 0o644));

    let sandbox = Arc::new(::fast_io::DirSandbox::open_root(&dest).expect("open sandbox"));

    let mut writer = TestDeletionWriter;
    let (stats, _exceeded, io_error_bits) = ctx
        .delete_extraneous_files(&dest, Some(&sandbox), &mut writer)
        .expect("delete pass must not surface io::Error on legitimate trees");

    assert_eq!(
        io_error_bits, 0,
        "ordinary subdir deletion must not set IOERR_GENERAL"
    );
    assert_eq!(stats.files, 1, "extraneous.txt should be deleted");
    assert!(
        !subdir.join("extraneous.txt").exists(),
        "extraneous.txt must be removed"
    );
    assert!(
        subdir.join("keep.txt").exists(),
        "keep.txt must survive a clean sweep"
    );
}

/// UTS-16.b.6 regression: the chdir-symlink-race attack window. An
/// attacker plants a symlink at `dest/symlinkattack` pointing outside
/// the destination root before the receiver's `--delete` scan runs.
/// The sandbox-anchored scan must refuse to descend through the
/// symlink and surface `IOERR_GENERAL` so the receiver returns
/// exit code 23 (`RERR_PARTIAL`) instead of completing silently
/// while leaving the attacker's symlink in place.
///
/// Additionally asserts the outside tree is untouched - the
/// symlink-refusal must close the syscall before any unlink can
/// land on the attacker-chosen inode.
///
/// upstream: clientserver.c:1018 `use_secure_symlinks` gates the
/// `do_*_at` wrappers in `syscall.c`; `secure_relative_open` returns
/// `errno=ELOOP` and the caller sets `io_error |= IOERR_GENERAL`.
#[cfg(unix)]
#[test]
fn delete_symlinked_subdir_surfaces_ioerr_general() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::generator::io_error_flags::{IOERR_GENERAL, to_exit_code};

    let temp_dir = TempDir::new().unwrap();
    let parent = std::fs::canonicalize(temp_dir.path()).unwrap();

    // The "sensitive" tree the attacker wants the receiver's scan
    // (and any follow-up unlink) to land on - sits outside the
    // destination root.
    let outside = parent.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let outside_sentinel = outside.join("sentinel");
    std::fs::write(&outside_sentinel, b"must-survive").unwrap();

    let dest = parent.join("dest");
    std::fs::create_dir(&dest).unwrap();

    // Attacker swaps the destination subdir for a symlink to the
    // sensitive tree above. The receiver's flist names `symlinkattack/`
    // as a real subdir, so the scan path matches a single-component
    // leaf under the sandbox root - the sandbox helper takes the
    // anchored fast path with O_NOFOLLOW and refuses the leaf.
    let attack_link = dest.join("symlinkattack");
    symlink(&outside, &attack_link).unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(
        std::str::from_utf8(dest.as_os_str().as_bytes()).unwrap(),
    )];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Sender's flist advertises `symlinkattack/` as a directory with a
    // child, so the deletion scan tries to enumerate the destination
    // subdir `dest/symlinkattack` to find extraneous entries. The
    // attacker-controlled destination is a symlink, so the
    // sandbox-anchored `read_dir` must refuse the leaf with ELOOP /
    // ENOTDIR rather than enumerating the outside tree.
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_directory("symlinkattack".into(), 0o755));
    ctx.file_list.push(FileEntry::new_file(
        "symlinkattack/keep.txt".into(),
        4,
        0o644,
    ));

    let sandbox = Arc::new(::fast_io::DirSandbox::open_root(&dest).expect("open sandbox"));

    let mut writer = TestDeletionWriter;
    let (_stats, _exceeded, io_error_bits) = ctx
        .delete_extraneous_files(&dest, Some(&sandbox), &mut writer)
        .expect("delete pass returns Ok with IOERR_GENERAL surfaced via bits");

    assert!(
        io_error_bits & IOERR_GENERAL != 0,
        "sandbox-rejected scan of a symlinked subdir must surface IOERR_GENERAL \
         (got 0x{io_error_bits:x})"
    );
    assert_eq!(
        to_exit_code(io_error_bits),
        23,
        "IOERR_GENERAL must map to exit code 23 (RERR_PARTIAL)"
    );

    // Defense-in-depth assertion: the outside tree must be untouched.
    // The sandbox helper closes the syscall at the O_NOFOLLOW probe
    // before any unlink dispatch, so the attacker-chosen inode never
    // sees a write.
    assert!(
        outside.is_dir(),
        "the outside tree must survive the refused scan"
    );
    assert!(
        outside_sentinel.is_file(),
        "the outside sentinel file must survive untouched"
    );
    assert_eq!(
        std::fs::read(&outside_sentinel).unwrap(),
        b"must-survive",
        "the outside sentinel contents must be untouched"
    );

    // The symlink itself stays in place - the scan refusal must not
    // implicitly unlink the attacker's symlink either; that decision
    // belongs to the explicit per-entry deletion path, which the
    // refusal short-circuited.
    assert!(
        attack_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "the planted symlink must remain in place (scan refusal closes the window without unlinking)"
    );
}
