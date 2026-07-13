//! Nextest port of the local legs of upstream `testsuite/chmod-option.test`.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/chmod-option.test`.
//!
//! # Background
//!
//! Upstream's `chmod-option.test` verifies that `--chmod` rewrites permission
//! bits on the fly during a transfer, applying the same symbolic-mode grammar
//! `chmod(1)` uses, with rsync's `F`/`D` file/directory qualifiers. Two of its
//! legs are pure local transfers with a deterministic outcome:
//!
//! 1. `--chmod ug-s,a+rX,D+w` - strip setuid/setgid, grant read plus
//!    conditional execute (`X` = execute only where already executable or on a
//!    directory), and give directories group/other write.
//! 2. `--chmod=Fo-x` - clear the world-execute bit on regular files only,
//!    leaving directories untouched.
//!
//! The remaining upstream legs drive a spawned `--daemon` with an
//! `incoming chmod` directive to reproduce a 2.6.8-era daemon bug; those need
//! a live listener and are out of scope for this in-process port.
//!
//! # Why this matters
//!
//! `--chmod` is applied by the receiver as it commits each entry. A regression
//! in the symbolic-mode parser or in the `F`/`D` qualifier dispatch silently
//! writes the wrong permission bits - a security-relevant divergence (e.g.
//! failing to strip setuid, or clearing execute on directories). The two legs
//! here pin the `X` conditional-execute rule, the setuid/setgid strip, the
//! directory-only `D` qualifier, and the regular-file-only `F` qualifier.
//!
//! Both legs assert the exact mode bits upstream rsync 3.4.4 produces (verified
//! against the installed upstream binary), so the test encodes the required
//! behaviour, not merely oc-rsync's current output.
//!
//! # Upstream References
//!
//! - `testsuite/chmod-option.test` - the upstream script this file ports.
//! - `chmod.c` - `parse_chmod()` symbolic-mode grammar and `F`/`D` qualifiers.
//! - `generator.c` - receiver-side application of the parsed chmod modes.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use test_support::{OcRsyncCliRunner, require_binary};

/// Low 12 permission bits (`0o7777`) of `path`, following the entry itself.
fn mode_bits(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

/// Trailing-slash form so rsync copies a directory's contents.
fn slash(p: &Path) -> std::ffi::OsString {
    let mut s = p.as_os_str().to_os_string();
    s.push("/");
    s
}

/// The self-lock and write-gated-restore legs only manifest for a non-root
/// transfer: root traverses any directory regardless of its mode, so upstream's
/// `!am_root` fixup gate is skipped and no self-lock occurs.
fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Leg 1: `--chmod ug-s,a+rX,D+w`.
///
/// Source modes: `name1` = 04700 (setuid, rwx------), `name2` = 0644,
/// `dir1` = 0700, `dir2` = 0770.
///
/// Expected destination modes (verified identical between oc-rsync and
/// upstream rsync 3.4.4):
/// - `name1`: strip setuid -> 0700, `a+rX` adds r for all and (already
///   executable) x for all -> 0755.
/// - `name2`: not executable, so `X` adds nothing; `a+r` leaves it -> 0644.
/// - `dir1`: 0700 -> `a+rX` -> 0755; `D+w` is a no-op here (0755).
/// - `dir2`: 0770 -> `a+rX` -> 0775; `D+w` keeps group/other write (0775).
#[test]
fn chmod_strip_setid_and_add_rx_dir_write() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let from = root.path().join("from");
    let to = root.path().join("to");
    fs::create_dir_all(&from).expect("mkdir from");

    fs::write(from.join("name1"), b"This is the file\n").expect("write name1");
    fs::write(from.join("name2"), b"This is the other file\n").expect("write name2");
    fs::create_dir(from.join("dir1")).expect("mkdir dir1");
    fs::create_dir(from.join("dir2")).expect("mkdir dir2");
    fs::set_permissions(from.join("name1"), fs::Permissions::from_mode(0o4700)).expect("chmod");
    fs::set_permissions(from.join("name2"), fs::Permissions::from_mode(0o644)).expect("chmod");
    fs::set_permissions(from.join("dir1"), fs::Permissions::from_mode(0o700)).expect("chmod");
    fs::set_permissions(from.join("dir2"), fs::Permissions::from_mode(0o770)).expect("chmod");

    let out = OcRsyncCliRunner::new()
        .args(["-a", "--chmod", "ug-s,a+rX,D+w"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        mode_bits(&to.join("name1")),
        0o755,
        "name1: setuid must be stripped and a+rX applied (0755)",
    );
    assert_eq!(
        mode_bits(&to.join("name2")),
        0o644,
        "name2: not executable, X adds nothing (0644)",
    );
    assert_eq!(
        mode_bits(&to.join("dir1")),
        0o755,
        "dir1: a+rX then D+w (0755)",
    );
    assert_eq!(
        mode_bits(&to.join("dir2")),
        0o775,
        "dir2: a+rX then D+w (0775)",
    );
}

/// Leg 2: `--chmod=Fo-x`.
///
/// `Fo-x` clears the world-execute bit on regular files only; directories are
/// left alone. Source: `bar` (a file, 0755 after `chmod o+x`), `foo` (a dir).
///
/// Expected destination modes (verified identical between oc-rsync and
/// upstream rsync 3.4.4):
/// - `bar`: source is 0645 (0644 + `o+x`); `Fo-x` clears world-execute -> 0644.
/// - `foo`: directory untouched by the `F`-only chmod -> 0755.
#[test]
fn chmod_file_only_clears_world_execute() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let from = root.path().join("from");
    let to = root.path().join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir(from.join("foo")).expect("mkdir foo");
    fs::write(from.join("bar"), b"").expect("touch bar");
    // Match upstream: `chmod o+x "$fromdir"/bar`. Start from 0644, add o+x.
    fs::set_permissions(from.join("bar"), fs::Permissions::from_mode(0o645)).expect("chmod bar");

    let out = OcRsyncCliRunner::new()
        .args(["-a", "--chmod=Fo-x"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        mode_bits(&to.join("bar")),
        0o644,
        "bar: Fo-x clears the world-execute bit on the regular file (0644)",
    );
    assert_eq!(
        mode_bits(&to.join("foo")),
        0o755,
        "foo: directory left untouched by the F-only chmod (0755)",
    );
}

/// Deliverable #1: a `--chmod` that strips the transfer-root directory's own
/// owner-execute bit self-locks it.
///
/// upstream: generator.c:1503-1520 - the generator chmods the transfer root
/// ("dst/.") to its tweaked mode and then tries to re-add owner-`rwx` so it can
/// write the root's contents. Resolving `.` inside the now owner-non-executable
/// root fails with `EACCES` ("failed to modify permissions on dst/."), the
/// generator can no longer stat or create the contents, so nothing transfers
/// and rsync exits 23 with the root left at the strict tweaked mode.
///
/// Verified against upstream rsync 3.4.4: `--chmod=ug=rw` leaves `dst` at 0o665,
/// `--chmod=a=r,g=w` at 0o424, `--chmod=u=r` at 0o455 - each exit 23 with an
/// empty destination. This encodes WHY the exit code matters: the spec makes the
/// destination root unreadable to its own owner, a partial-transfer failure a
/// script must observe, not silently succeed.
#[test]
fn chmod_transfer_root_self_locks_without_owner_execute() {
    if !require_binary("oc-rsync") || running_as_root() {
        return;
    }
    for (spec, expected_mode) in [("ug=rw", 0o665u32), ("a=r,g=w", 0o424), ("u=r", 0o455)] {
        let root = tempfile::tempdir().expect("tempdir");
        let from = root.path().join("from");
        let to = root.path().join("to");
        fs::create_dir_all(&from).expect("mkdir from");
        fs::write(from.join("f"), b"hello\n").expect("write f");
        fs::set_permissions(&from, fs::Permissions::from_mode(0o755)).expect("chmod from");
        fs::set_permissions(from.join("f"), fs::Permissions::from_mode(0o644)).expect("chmod f");

        let out = OcRsyncCliRunner::new()
            .args(["-a", "--chmod"])
            .arg(spec)
            .arg(slash(&from))
            .arg(slash(&to))
            .run()
            .expect("run oc-rsync");

        out.assert_exit(23);
        assert!(
            out.stderr_contains("modify permissions"),
            "spec {spec}: expected a self-lock permission error, got:\n{}",
            out.stderr_str(),
        );
        assert_eq!(
            mode_bits(&to),
            expected_mode,
            "spec {spec}: transfer root must be left at the strict tweaked mode",
        );

        // Restore owner-execute so TempDir can traverse and clean up.
        fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).expect("restore to");
        let contents: Vec<_> = fs::read_dir(&to)
            .expect("read to")
            .map(|e| e.expect("dirent").file_name())
            .collect();
        assert!(
            contents.is_empty(),
            "spec {spec}: a self-locked root transfers no contents, found {contents:?}",
        );
    }
}

/// Deliverable #2: a subdirectory made owner-non-executable but owner-writable
/// by `--chmod` regains owner-execute; owner-non-writable ones keep the strict
/// mode.
///
/// upstream: generator.c:1512-1520 raises a directory to owner-`rwx` while its
/// contents are written, and generator.c:2107-2145 `touch_up_dirs()` restores
/// the tweaked mode ONLY when the owner would otherwise lack write. A subdir is
/// addressed by name (not "./"), so the re-add chmod always succeeds; the net
/// on-disk mode therefore keeps the transient owner bits when the owner is
/// writable.
///
/// Verified against upstream rsync 3.4.4 for `src -> dst/src`: `--chmod=ug=rw`
/// leaves `dst/src` at 0o765 (owner-write kept 0o700), while `--chmod=u=r`
/// leaves it at 0o455 (owner not writable, strict mode restored).
#[test]
fn chmod_subdir_owner_writable_regains_execute() {
    if !require_binary("oc-rsync") || running_as_root() {
        return;
    }
    for (spec, expected_mode) in [("ug=rw", 0o765u32), ("u=r", 0o455)] {
        let root = tempfile::tempdir().expect("tempdir");
        let src = root.path().join("src");
        let dst = root.path().join("dst");
        fs::create_dir_all(&src).expect("mkdir src");
        fs::write(src.join("f"), b"hello\n").expect("write f");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).expect("chmod src");
        fs::set_permissions(src.join("f"), fs::Permissions::from_mode(0o644)).expect("chmod f");
        fs::create_dir(&dst).expect("mkdir dst");

        // No trailing slash on src: the transfer root is dst/src, a named
        // subdirectory that never self-locks.
        let out = OcRsyncCliRunner::new()
            .args(["-a", "--chmod"])
            .arg(spec)
            .arg(&src)
            .arg(slash(&dst))
            .run()
            .expect("run oc-rsync");

        out.assert_success();
        assert_eq!(
            mode_bits(&dst.join("src")),
            expected_mode,
            "spec {spec}: dst/src final mode must match upstream's during-transfer dance",
        );
        // Restore owner-execute before probing contents: a 0o455 result is not
        // traversable by its owner, so `exists()` would spuriously fail.
        fs::set_permissions(dst.join("src"), fs::Permissions::from_mode(0o755)).expect("restore");
        assert!(
            dst.join("src/f").exists(),
            "spec {spec}: subdir contents must transfer (the dir was owner-rwx while writing)",
        );
    }
}
