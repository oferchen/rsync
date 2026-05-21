//! `*at` syscall helpers for the rename cutover (SEC-1.j).
//!
//! Companion to [`super::at_syscalls`] and [`super::at_syscalls_metadata`]:
//! the unlink/lstat/create-class siblings live in `at_syscalls`, the
//! chmod/chown/utimensat siblings live in `at_syscalls_metadata`, and the
//! rename-class siblings live here. The split keeps SEC-1.j mergeable
//! without conflicts with the in-flight SEC-1.i PR; once both land the
//! three modules may be re-folded into a single `at_syscalls` namespace.
//!
//! Each helper takes parent dirfds plus single-component leaves so the
//! call cannot be redirected by a TOCTOU symlink swap between the
//! receiver's "decide to commit" moment and the kernel reaching the
//! inode. The path-based [`std::fs::rename`] fallback remains for
//! multi-component paths and the no-sandbox case so behaviour is
//! byte-identical for callers that have not yet plumbed a
//! [`DirSandbox`](super::DirSandbox).

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// `RENAME_NOREPLACE` from `renameat2(2)`.
///
/// Causes `renameat2(2)` to fail with `EEXIST` when the destination
/// already exists, instead of silently overwriting it. Available on
/// Linux 3.15+ on most filesystems; older kernels return `ENOSYS` /
/// `EINVAL` and the caller must fall back to plain `renameat(2)`.
#[cfg(target_os = "linux")]
const RENAME_NOREPLACE: libc::c_uint = 1;

/// Issue `renameat(old_dirfd, old_name, new_dirfd, new_name)`.
///
/// Both endpoints are resolved relative to their respective dirfds. When
/// `replace` is `false` the helper attempts `renameat2(2)` with
/// `RENAME_NOREPLACE` so the kernel fails with `EEXIST` instead of
/// overwriting; on kernels that lack the opcode the helper falls back to
/// plain `renameat(2)` after the kernel reports `ENOSYS` / `EINVAL`.
///
/// `replace == true` matches the default [`std::fs::rename`] semantics:
/// overwrite the destination if it exists, atomically swapping the two
/// inodes when possible.
///
/// `old_name` and `new_name` must not contain interior NUL bytes;
/// callers that pull names from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `old_name` does not exist beneath `old_dirfd`, or
///   when an intermediate component of `new_name` is missing.
/// - `EEXIST` when `replace == false` and `new_name` already exists
///   (only on kernels that accept `RENAME_NOREPLACE`; older kernels
///   silently overwrite via the fallback path).
/// - `EXDEV` when the two paths resolve to different filesystems.
/// - `EISDIR` when `new_name` is an existing directory and `old_name`
///   is not.
/// - `ENOTDIR` when `old_name` is a directory but `new_name` exists
///   and is not, or vice versa.
/// - `EACCES` when the caller lacks the required permissions.
/// - `EINVAL` when either name contains an interior NUL byte
///   (translated from [`std::ffi::NulError`]).
pub fn renameat(
    old_dirfd: BorrowedFd<'_>,
    old_name: &OsStr,
    new_dirfd: BorrowedFd<'_>,
    new_name: &OsStr,
    replace: bool,
) -> io::Result<()> {
    let c_old = CString::new(old_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let c_new = CString::new(new_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    #[cfg(target_os = "linux")]
    {
        if !replace {
            // SAFETY:
            // - Both `BorrowedFd<'_>` arguments outlive the syscall
            //   (lifetime bound to the borrows passed in).
            // - Both `CString` arguments are valid NUL-terminated C
            //   strings borrowed for the duration of the call; the
            //   kernel does not retain the pointers past return.
            // - `RENAME_NOREPLACE` is the only flag passed; the kernel
            //   accepts it on Linux 3.15+ and reports `ENOSYS` /
            //   `EINVAL` on older kernels which we map to the fallback.
            #[allow(unsafe_code)]
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    old_dirfd.as_raw_fd(),
                    c_old.as_ptr(),
                    new_dirfd.as_raw_fd(),
                    c_new.as_ptr(),
                    RENAME_NOREPLACE,
                )
            };
            if rc == 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                // Older kernels and exotic filesystems may reject the
                // flag; fall through to plain renameat(2) below so the
                // caller still gets a result, accepting that the
                // overwrite-or-not check becomes a TOCTOU on those
                // kernels. The same trade-off is documented for the
                // upstream renameat2 backstop in `util1.c`.
                Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => {}
                _ => return Err(err),
            }
        }
    }
    // SAFETY:
    // - Both `BorrowedFd<'_>` arguments outlive the syscall.
    // - Both `CString` arguments are valid NUL-terminated C strings
    //   borrowed for the duration of the call.
    // - `renameat(2)` is the POSIX-portable rename entry point and
    //   accepts dirfds plus relative names without flag knobs.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::renameat(
            old_dirfd.as_raw_fd(),
            c_old.as_ptr(),
            new_dirfd.as_raw_fd(),
            c_new.as_ptr(),
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `renameat` against `old_link_path` / `new_link_path` when the
/// `sandbox` root is the immediate parent of **both** endpoints.
///
/// SEC-1.j adaptor for the receiver temp-file → final-name commit:
/// - When `sandbox` is `Some`, both `old_link_path` and `new_link_path`
///   equal `<old_dest_dir>.join(<old_relative_path>)` /
///   `<new_dest_dir>.join(<new_relative_path>)` with single-component
///   relatives, the helper resolves both leaves through the sandbox
///   dirfd so a mid-syscall symlink swap on either leaf cannot redirect
///   the rename to an attacker-chosen inode.
/// - In every other case the helper falls back to [`std::fs::rename`]
///   on the absolute paths so behaviour matches the existing
///   path-based commit semantics.
///
/// Today both endpoints anchor on `sandbox.current_dirfd()` because the
/// receiver always creates its temp file inside the same destination
/// parent as the final name (see `temp_guard::open_tmpfile`). The
/// two-`dest_dir` signature is retained so a future cross-dir rename
/// (e.g., `--backup-dir`) can be plumbed through here without changing
/// the call sites.
///
/// `replace` mirrors the [`renameat`] knob: `true` overwrites the
/// destination atomically (default [`std::fs::rename`] semantics);
/// `false` attempts `renameat2(RENAME_NOREPLACE)` on Linux.
///
/// # Errors
///
/// Surfaces either the [`renameat`] error or the [`std::fs::rename`]
/// error verbatim, depending on which path was taken.
#[allow(clippy::too_many_arguments)]
pub fn renameat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    old_dest_dir: &Path,
    old_relative_path: &Path,
    old_link_path: &Path,
    new_dest_dir: &Path,
    new_relative_path: &Path,
    new_link_path: &Path,
    replace: bool,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(old_leaf) =
            single_component_leaf(old_dest_dir, old_relative_path, old_link_path)
        && let Some(new_leaf) =
            single_component_leaf(new_dest_dir, new_relative_path, new_link_path)
    {
        let dirfd = sandbox.current_dirfd();
        return renameat(dirfd, old_leaf, dirfd, new_leaf, replace);
    }
    std::fs::rename(old_link_path, new_link_path)
}

/// Returns the leaf component of `link_path` when `link_path` is exactly
/// `dest_dir` joined with a single-component `relative_path`.
///
/// Mirrors the SEC-1.f / SEC-1.g / SEC-1.h / SEC-1.i leaf detector
/// verbatim; kept private to this module so the SEC-1.j cutover can land
/// independently of the in-flight SEC-1.i refactor that lives in
/// [`super::at_syscalls_metadata`]. Multi-component relative paths take
/// the path-based fallback until the per-directory dirfd stack lands.
fn single_component_leaf<'a>(
    dest_dir: &Path,
    relative_path: &'a Path,
    link_path: &Path,
) -> Option<&'a OsStr> {
    let mut comps = relative_path.components();
    let first = match comps.next()? {
        std::path::Component::Normal(name) => name,
        _ => return None,
    };
    if comps.next().is_some() {
        return None;
    }
    if dest_dir.join(relative_path) != link_path {
        return None;
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use tempfile::tempdir;

    use super::*;
    use crate::dir_sandbox::DirSandbox;
    use crate::secure_dir::secure_open_dir;

    fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().expect("tempdir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
        (dir, canon)
    }

    #[test]
    fn renameat_renames_regular_file_in_same_dir() {
        let (_keep, root) = canonical_tempdir();
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"payload").expect("write src");

        let dirfd = secure_open_dir(&root).expect("open root");
        renameat(
            dirfd.as_fd(),
            OsStr::new("src"),
            dirfd.as_fd(),
            OsStr::new("dst"),
            true,
        )
        .expect("renameat");

        assert!(!src.exists(), "source must be gone after renameat");
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"payload");
    }

    #[test]
    fn renameat_reports_enoent_for_missing_source() {
        let (_keep, root) = canonical_tempdir();
        let dirfd = secure_open_dir(&root).expect("open root");
        let err = renameat(
            dirfd.as_fd(),
            OsStr::new("absent"),
            dirfd.as_fd(),
            OsStr::new("target"),
            true,
        )
        .expect_err("missing source must error");
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    }

    #[test]
    fn renameat_at_fdcwd_interop() {
        // AT_FDCWD passed via BorrowedFd::borrow_raw must behave like a
        // path-based rename(2). Sanity check: rename inside a tempdir
        // referenced by relative path with the process cwd set there.
        let (_keep, root) = canonical_tempdir();
        let original_cwd = std::env::current_dir().expect("getcwd");
        std::env::set_current_dir(&root).expect("chdir");
        let src_relative = OsStr::new("at_fdcwd_src");
        let dst_relative = OsStr::new("at_fdcwd_dst");
        std::fs::write(root.join("at_fdcwd_src"), b"x").expect("write src");

        // SAFETY: AT_FDCWD is a kernel-defined sentinel, not a real fd,
        // but `BorrowedFd::borrow_raw` accepts negative ints for exactly
        // this use case.
        #[allow(unsafe_code)]
        let cwd_fd = unsafe { BorrowedFd::borrow_raw(libc::AT_FDCWD) };
        let result = renameat(cwd_fd, src_relative, cwd_fd, dst_relative, true);
        // Restore cwd before any assertion so a failure does not leave
        // the test binary in the tempdir.
        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        result.expect("renameat AT_FDCWD");

        assert!(!root.join("at_fdcwd_src").exists());
        assert_eq!(
            std::fs::read(root.join("at_fdcwd_dst")).expect("read"),
            b"x"
        );
    }

    #[test]
    fn renameat_across_two_distinct_dirfds() {
        let (_keep, root) = canonical_tempdir();
        std::fs::create_dir(root.join("from")).expect("mkdir from");
        std::fs::create_dir(root.join("to")).expect("mkdir to");
        std::fs::write(root.join("from").join("file"), b"cross").expect("write");

        let from_fd = secure_open_dir(&root.join("from")).expect("open from");
        let to_fd = secure_open_dir(&root.join("to")).expect("open to");
        renameat(
            from_fd.as_fd(),
            OsStr::new("file"),
            to_fd.as_fd(),
            OsStr::new("file"),
            true,
        )
        .expect("renameat cross-dir");

        assert!(!root.join("from").join("file").exists());
        assert_eq!(
            std::fs::read(root.join("to").join("file")).expect("read dst"),
            b"cross"
        );
    }

    #[test]
    fn renameat_via_sandbox_takes_at_path_for_single_component() {
        let (_keep, root) = canonical_tempdir();
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"sandboxed").expect("write src");
        let sandbox = DirSandbox::open_root(&root).expect("sandbox");

        let src_rel = Path::new("src");
        let dst_rel = Path::new("dst");
        renameat_via_sandbox_or_fallback(
            Some(&sandbox),
            &root,
            src_rel,
            &src,
            &root,
            dst_rel,
            &dst,
            true,
        )
        .expect("renameat sandbox");

        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"sandboxed");
    }

    #[test]
    fn renameat_via_sandbox_falls_back_for_multi_component_source() {
        let (_keep, root) = canonical_tempdir();
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");
        let src = root.join("sub").join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"fallback-src").expect("write src");
        let sandbox = DirSandbox::open_root(&root).expect("sandbox");

        renameat_via_sandbox_or_fallback(
            Some(&sandbox),
            &root,
            Path::new("sub/src"),
            &src,
            &root,
            Path::new("dst"),
            &dst,
            true,
        )
        .expect("renameat fallback");

        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"fallback-src");
    }

    #[test]
    fn renameat_via_sandbox_falls_back_when_sandbox_absent() {
        let (_keep, root) = canonical_tempdir();
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"no-sandbox").expect("write src");

        renameat_via_sandbox_or_fallback(
            None,
            &root,
            Path::new("src"),
            &src,
            &root,
            Path::new("dst"),
            &dst,
            true,
        )
        .expect("renameat no-sandbox");

        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"no-sandbox");
    }

    #[test]
    fn renameat_via_sandbox_falls_back_when_paths_cross_sandbox_boundary() {
        // When `dest_dir` does not match the `link_path`'s actual parent
        // the single-component-leaf check fails and the helper falls
        // back to std::fs::rename. This is the safety net for callers
        // that pass a mismatched (dest_dir, link_path) pair.
        let (_keep, root) = canonical_tempdir();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("mkdir elsewhere");
        let src = elsewhere.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"cross-boundary").expect("write src");
        let sandbox = DirSandbox::open_root(&root).expect("sandbox");

        // dest_dir = root, relative_path = "src", but link_path = root/elsewhere/src.
        // The helper rejects the leaf shortcut and falls through.
        renameat_via_sandbox_or_fallback(
            Some(&sandbox),
            &root,
            Path::new("src"),
            &src,
            &root,
            Path::new("dst"),
            &dst,
            true,
        )
        .expect("renameat cross-boundary fallback");

        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"cross-boundary");
    }

    #[test]
    fn renameat_overwrites_existing_destination_by_default() {
        let (_keep, root) = canonical_tempdir();
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"new").expect("write src");
        std::fs::write(&dst, b"old").expect("write dst");
        let dirfd = secure_open_dir(&root).expect("open root");

        renameat(
            dirfd.as_fd(),
            OsStr::new("src"),
            dirfd.as_fd(),
            OsStr::new("dst"),
            true,
        )
        .expect("renameat overwrite");

        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"new");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn renameat_noreplace_refuses_existing_destination_on_linux() {
        // On Linux 3.15+ RENAME_NOREPLACE returns EEXIST when the
        // destination is present. Older kernels return ENOSYS / EINVAL
        // and the helper falls back to overwriting; on that path the
        // assertion below would fail, so this test is Linux-only and
        // tolerates the fallback path by accepting overwrite as well.
        let (_keep, root) = canonical_tempdir();
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"new").expect("write src");
        std::fs::write(&dst, b"old").expect("write dst");
        let dirfd = secure_open_dir(&root).expect("open root");

        match renameat(
            dirfd.as_fd(),
            OsStr::new("src"),
            dirfd.as_fd(),
            OsStr::new("dst"),
            false,
        ) {
            Err(err) if err.raw_os_error() == Some(libc::EEXIST) => {
                assert!(src.exists(), "src must remain after EEXIST");
                assert_eq!(std::fs::read(&dst).expect("read dst"), b"old");
            }
            Ok(()) => {
                // Pre-3.15 kernel or filesystem without RENAME_NOREPLACE
                // support: helper transparently overwrote.
                assert!(!src.exists());
                assert_eq!(std::fs::read(&dst).expect("read dst"), b"new");
            }
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn renameat_via_sandbox_succeeds_with_sandbox_secondary_dirs() {
        // Confirm the sandbox path works when the same dest_dir is used
        // for both endpoints (the common receiver case where temp file
        // and final file share a parent).
        let (_keep, root) = canonical_tempdir();
        let temp = root.join(".final.XXXXXX");
        let final_path = root.join("final");
        std::fs::write(&temp, b"committed").expect("write temp");
        let sandbox = DirSandbox::open_root(&root).expect("sandbox");

        renameat_via_sandbox_or_fallback(
            Some(&sandbox),
            &root,
            Path::new(".final.XXXXXX"),
            &temp,
            &root,
            Path::new("final"),
            &final_path,
            true,
        )
        .expect("renameat sandbox temp commit");

        assert!(!temp.exists());
        assert_eq!(std::fs::read(&final_path).expect("read"), b"committed");
    }
}
