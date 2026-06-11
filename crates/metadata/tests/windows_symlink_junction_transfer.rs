//! End-to-end symlink + junction transfer round-trip through oc-rsync on
//! Windows (WPC-9'.5 and WPC-9'.6).
//!
//! Sibling tests in `windows_reparse_fixtures.rs` (PR #5583) feed the WPC-8
//! classifier (`metadata::windows::reparse::classify_reparse_point`) with
//! realistic `mklink`-created reparse points and assert classifier output.
//! Those tests stop at the kernel boundary: they prove the classifier reads
//! the right tag, but never exercise the receiver, generator, or sender.
//!
//! This module drives the full push pipeline (file-list, delta, finalize) by
//! invoking `oc-rsync.exe --archive --links src/ dst/` as a subprocess and
//! asserting that:
//!
//! - A directory symbolic link (`mklink /d`, `IO_REPARSE_TAG_SYMLINK`)
//!   survives transfer as a symlink on the destination, NOT as a recursive
//!   copy of the target directory.
//! - A directory junction (`mklink /j`, `IO_REPARSE_TAG_MOUNT_POINT`) lands
//!   on the destination with the same reparse classification, NOT as a
//!   symlink and NOT as a recursive copy.
//!
//! ## Skip conditions
//!
//! - `mklink /d` requires `SeCreateSymbolicLinkPrivilege`, available with
//!   administrator privileges or Windows 10 developer mode. The symlink
//!   test downgrades to a runtime skip when the privilege is missing so the
//!   suite stays green on stock GitHub-hosted `windows-latest` runners
//!   (which run as a non-admin user with developer mode disabled).
//! - `mklink /j` works without elevation on Windows 10+, so the junction
//!   test runs unconditionally; it skips only if the `oc-rsync.exe` binary
//!   cannot be located.
//! - The `oc-rsync.exe` binary location is probed through
//!   `CARGO_BIN_EXE_oc-rsync` (set by cargo when tests live in the binary's
//!   owning package; the metadata crate is not the binary's owner so this
//!   is best-effort) and falls back to `target/{release,debug,dist}/oc-rsync.exe`
//!   relative to the workspace root.
//!
//! ## CI wire-up
//!
//! The GitHub-hosted `windows-latest` runner used by `ci.yml` runs as a
//! non-admin user with developer mode disabled, so the symlink test
//! exercises only the skip path there. The junction case still exercises
//! the full transfer path. Tracked under the WPC-9 follow-up: lighting up
//! the symlink test in CI requires either a self-hosted runner with
//! developer mode enabled or invoking `oc-rsync` under an elevated shell.
//!
//! ## Why duplicate the fixture wrappers
//!
//! `tests/windows_reparse_fixtures.rs` (PR #5583) defines
//! `DirSymlinkFixture` and `JunctionFixture` but each `.rs` file under
//! `tests/` compiles as a separate integration-test crate, so the types
//! cannot be imported across files without `#[path]` inclusion (which
//! would double-compile the fixture file's own `#[test]` items). The
//! fixtures here are trimmed copies that delegate to the same `mklink`
//! commands and exhibit the same RAII cleanup semantics.

#![cfg(target_os = "windows")]

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

/// Minimal RAII wrapper around `cmd.exe /c mklink /d <link> <target>`.
///
/// Returns [`io::ErrorKind::PermissionDenied`] when `mklink /d` exits
/// non-zero (most commonly: caller lacks `SeCreateSymbolicLinkPrivilege`).
/// Mirrors `DirSymlinkFixture` in `windows_reparse_fixtures.rs`; see the
/// module docstring for why the type is duplicated here.
struct DirSymlinkFixture {
    link: PathBuf,
}

impl DirSymlinkFixture {
    fn new(link: &Path, target: &Path) -> io::Result<Self> {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/d"])
            .arg(link)
            .arg(target)
            .status()?;
        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mklink /d requires administrator or developer mode",
            ));
        }
        Ok(Self {
            link: link.to_path_buf(),
        })
    }
}

impl Drop for DirSymlinkFixture {
    fn drop(&mut self) {
        // Windows treats directory symlinks as directories at the
        // filesystem layer; `remove_dir` deletes the reparse point itself
        // without recursing into the target.
        let _ = fs::remove_dir(&self.link);
    }
}

/// Minimal RAII wrapper around `cmd.exe /c mklink /j <link> <target>`.
///
/// `mklink /j` does not require elevation on Windows 10+, so this
/// constructor only fails when `cmd.exe` is unavailable or the underlying
/// filesystem rejects the operation.
struct JunctionFixture {
    link: PathBuf,
}

impl JunctionFixture {
    fn new(link: &Path, target: &Path) -> io::Result<Self> {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/j"])
            .arg(link)
            .arg(target)
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "mklink /j {} {} exited with {status:?}",
                link.display(),
                target.display()
            )));
        }
        Ok(Self {
            link: link.to_path_buf(),
        })
    }
}

impl Drop for JunctionFixture {
    fn drop(&mut self) {
        // Junctions are directory reparse points; `remove_dir` removes the
        // reparse without recursing into the target.
        let _ = fs::remove_dir(&self.link);
    }
}

/// Locate the `oc-rsync.exe` binary built for this workspace.
///
/// Order:
/// 1. `CARGO_BIN_EXE_oc-rsync` (set by cargo for tests living in the
///    binary's owning package; metadata tests do not get this for free, so
///    this branch is best-effort).
/// 2. `target/{release,debug,dist}/oc-rsync.exe` relative to the workspace
///    root, walked from `CARGO_MANIFEST_DIR` (the metadata crate
///    directory) up to the workspace root.
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent()?.parent()?;
    for profile in ["release", "debug", "dist"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join("oc-rsync.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Logs a skip reason and returns cleanly. Matches the convention in
/// `windows_ads_xattrs_roundtrip.rs`.
fn skip(reason: &str) {
    eprintln!("skipped: {reason}");
}

/// Spawn `oc-rsync --archive --links <src>/ <dst>/` and assert success.
///
/// `--archive` enables `-rlptgoD`, which already implies `--links`; the
/// explicit `--links` here documents intent for the symlink + junction
/// cases. Trailing slashes mirror the upstream `src/` convention for
/// "copy the contents of src into dst", which is what the assertions
/// downstream depend on.
fn run_oc_rsync_push(oc: &Path, src: &Path, dst: &Path) {
    let src_arg = format!("{}\\", src.display());
    let dst_arg = format!("{}\\", dst.display());
    let output = Command::new(oc)
        .args(["--archive", "--links"])
        .arg(&src_arg)
        .arg(&dst_arg)
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "oc-rsync push exited with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Verify oc-rsync push preserves a directory symbolic link (`mklink /d`).
///
/// Sets up `src/real_dir/inside.txt` and `src/link_to_real -> real_dir`,
/// pushes through `oc-rsync --archive --links`, and asserts that:
///
/// 1. `dst/link_to_real` exists and is itself a symlink (so the transfer
///    did not dereference into a recursive copy of `real_dir`).
/// 2. `dst/link_to_real/inside.txt` is readable through the link with the
///    original payload, proving the link target resolved correctly on the
///    destination.
///
/// Requires `SeCreateSymbolicLinkPrivilege` (administrator or developer
/// mode); skips cleanly otherwise.
#[test]
fn dir_symlink_push_preserves_link() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");

    let target = src.join("real_dir");
    fs::create_dir(&target).expect("create target dir");
    fs::write(target.join("inside.txt"), b"hello").expect("write inside.txt");

    let link = src.join("link_to_real");
    let _fixture = match DirSymlinkFixture::new(&link, &target) {
        Ok(f) => f,
        Err(err) => {
            skip(&format!("mklink /d unavailable ({err})"));
            return;
        }
    };

    let oc = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            skip("oc-rsync binary not found");
            return;
        }
    };

    run_oc_rsync_push(&oc, &src, &dst);

    let dst_link = dst.join("link_to_real");
    let metadata = fs::symlink_metadata(&dst_link).expect("symlink_metadata on transferred link");
    assert!(
        metadata.file_type().is_symlink(),
        "expected symlink at {dst_link:?}, got file_type={:?}",
        metadata.file_type(),
    );

    let dst_target = dst.join("real_dir");
    assert!(
        dst_target.is_dir(),
        "expected real_dir to land on dst as a directory, got {dst_target:?}",
    );

    let inside = fs::read_to_string(dst_link.join("inside.txt"))
        .expect("read inside.txt through transferred symlink");
    assert_eq!(
        inside, "hello",
        "symlink target on destination resolved to wrong content",
    );
}

/// Verify oc-rsync push preserves a directory junction (`mklink /j`).
///
/// Sets up `src/real_dir/inside.txt` and `src/junction_to_real -> real_dir`
/// using `mklink /j` (no elevation required), pushes through
/// `oc-rsync --archive --links`, and asserts that the destination entry is
/// itself a reparse point (`file_type().is_symlink()` is true on Windows
/// for any reparse-tagged directory, covering both junctions and
/// symlinks). The transfer must not silently dereference the junction
/// into a recursive copy of the target.
///
/// The deeper classifier-level assertion (junction stays a junction, not
/// a symlink) lives in `windows_reparse_fixtures.rs` and depends on the
/// WPC-8 classifier from PR #5579 / PR #5592; this end-to-end test just
/// proves the transfer pipeline preserves the reparse-point shape.
#[test]
fn junction_push_preserves_junction() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");

    let target = src.join("real_dir");
    fs::create_dir(&target).expect("create target dir");
    fs::write(target.join("inside.txt"), b"hello").expect("write inside.txt");

    let link = src.join("junction_to_real");
    let _fixture = match JunctionFixture::new(&link, &target) {
        Ok(f) => f,
        Err(err) => {
            skip(&format!("mklink /j unavailable ({err})"));
            return;
        }
    };

    let oc = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            skip("oc-rsync binary not found");
            return;
        }
    };

    run_oc_rsync_push(&oc, &src, &dst);

    let dst_link = dst.join("junction_to_real");
    let metadata =
        fs::symlink_metadata(&dst_link).expect("symlink_metadata on transferred junction");
    assert!(
        metadata.file_type().is_symlink(),
        "expected reparse point at {dst_link:?}, got file_type={:?}; \
         a junction must not be silently dereferenced into a directory copy",
        metadata.file_type(),
    );

    let dst_target = dst.join("real_dir");
    assert!(
        dst_target.is_dir(),
        "expected real_dir to land on dst as a directory, got {dst_target:?}",
    );

    let inside = fs::read_to_string(dst_link.join("inside.txt"))
        .expect("read inside.txt through transferred junction");
    assert_eq!(
        inside, "hello",
        "junction target on destination resolved to wrong content",
    );
}
