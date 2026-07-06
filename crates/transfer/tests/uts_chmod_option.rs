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
