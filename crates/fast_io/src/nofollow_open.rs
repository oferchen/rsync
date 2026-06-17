//! Receiver-side basis file open with `O_NOFOLLOW` on the basename.
//!
//! Mirrors upstream rsync's `do_open_at()` / `secure_relative_open()`
//! dirname/basename split in `syscall.c:705` and `syscall.c:1769`: the
//! parent directory is opened with normal symlink resolution (so a
//! legitimate directory symlink such as the one created by
//! `--copy-dirlinks` continues to work) while the final path component
//! is opened with `openat(dirfd, basename, O_RDONLY | O_NOFOLLOW)` so a
//! pre-planted symlinked basename cannot redirect the basis read to an
//! attacker-chosen file.
//!
//! The receiver basis lookup is the call site this helper exists for:
//! it must follow directory symlinks (issue #715 regression test
//! `symlink-dirlink-basis.test`) while still refusing to follow a
//! symlinked leaf component. Path-confinement (`RESOLVE_BENEATH`) is
//! handled separately by [`crate::secure_dir::secure_open_dir`] and
//! [`crate::DirSandbox`]; this helper deliberately does not enforce it,
//! because the receiver already resolves the destination root before
//! reaching the basis lookup.
//!
//! # Platform behaviour
//!
//! - Unix: dirname/basename split with `O_NOFOLLOW` on the basename via
//!   `openat(2)`. Top-level paths (no slash) bypass the split and call
//!   `open(2)` directly, matching upstream's `if (!slash) return
//!   do_open(...)` short-circuit.
//! - Windows: falls back to [`std::fs::File::open`]. The standard NTFS
//!   open path does not auto-follow reparse-point symlinks in a way that
//!   the receiver tree creates, and the rsync upstream guarantee is
//!   limited to platforms with `O_NOFOLLOW`. See the `WPC-*` audit for
//!   the broader Windows symlink/reparse story.

use std::fs::File;
use std::io;
use std::path::Path;

/// Open `path` for reading with upstream `do_open_at()` semantics: the
/// parent directory is resolved normally (symlinks followed) and the
/// basename is opened with `O_NOFOLLOW` so a symlinked leaf component
/// is rejected with `ELOOP`.
///
/// Top-level paths (no `/`) short-circuit to [`File::open`] because
/// there is no dirname to split, matching upstream `syscall.c:727`.
///
/// # Errors
///
/// - `ELOOP` (`io::ErrorKind::FilesystemLoop` on Rust 1.78+ where
///   available, otherwise the raw OS error) when the basename is a
///   symlink.
/// - Any other I/O error from opening the parent directory or the
///   basename (forwarded verbatim from the underlying syscall).
pub fn open_basis_nofollow(path: &Path) -> io::Result<File> {
    imp::open_basis_nofollow(path)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::fs::OpenOptionsExt;

    use crate::dir_sandbox::openat;

    pub(super) fn open_basis_nofollow(path: &Path) -> io::Result<File> {
        let Some(basename) = path.file_name() else {
            // No basename (e.g. "/" or ""): defer to the plain open and
            // let the kernel report the appropriate error. Mirrors
            // upstream's pass-through for degenerate inputs.
            return File::open(path);
        };

        // Upstream `syscall.c:727`: `if (!slash) return do_open(...)`.
        // Treat the empty parent ("foo" with no slash) and the lone
        // root component identically: there is no dirname to split.
        let dirname = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => return File::open(path),
        };

        let dir = open_dir_follow(dirname)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        openat(dir.as_fd(), basename, flags, 0)
    }

    /// Open `dirname` as a directory file descriptor with normal symlink
    /// resolution. Legitimate directory symlinks (e.g. created by
    /// `--copy-dirlinks` on the receiver) must be followed, so this
    /// deliberately does not use `O_NOFOLLOW`.
    fn open_dir_follow(dirname: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(dirname)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub(super) fn open_basis_nofollow(path: &Path) -> io::Result<File> {
        // Windows / other non-Unix: NTFS reparse-point resolution is
        // governed by separate flags on `CreateFileW` and is not part
        // of the rsync upstream `O_NOFOLLOW` contract. Fall through to
        // the standard open; receiver-side reparse handling is audited
        // under WPC-3/4.
        File::open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    /// Test 1 mirror: basis file at `<temp>/real-dir/basis` reached via
    /// the directory symlink `<temp>/dir -> real-dir`. The receiver must
    /// open it. This is the `symlink-dirlink-basis.test` regression.
    #[cfg(unix)]
    #[test]
    fn opens_basis_through_directory_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real_dir = tmp.path().join("real-dir");
        std::fs::create_dir(&real_dir).expect("mkdir real-dir");
        let basis_path = real_dir.join("basis");
        std::fs::write(&basis_path, b"hello").expect("write basis");

        let dir_link = tmp.path().join("dir");
        symlink("real-dir", &dir_link).expect("symlink dir -> real-dir");

        let through_link = dir_link.join("basis");
        let mut file = open_basis_nofollow(&through_link).expect("open via dir symlink");
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut file, &mut buf).expect("read");
        assert_eq!(buf, "hello");
    }

    /// Negative: basis basename is itself a symlink. The receiver must
    /// refuse to follow it (matches upstream's `O_NOFOLLOW` on the leaf
    /// component in `do_open_at()` / `secure_relative_open()`).
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_basename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("secret");
        std::fs::write(&target, b"do-not-leak").expect("write target");

        let dir = tmp.path().join("dir");
        std::fs::create_dir(&dir).expect("mkdir");
        let basis = dir.join("basis");
        symlink(&target, &basis).expect("symlink basis -> secret");

        let err = open_basis_nofollow(&basis).expect_err("must not follow symlinked basename");
        // `ELOOP` is the canonical errno for an `O_NOFOLLOW` refusal.
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
    }

    /// Nested directory symlinks (test 3 mirror):
    /// `<temp>/nested -> nested_real`, basis at
    /// `<temp>/nested_real/sub/data`.
    #[cfg(unix)]
    #[test]
    fn opens_basis_through_nested_directory_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested_real_sub = tmp.path().join("nested_real").join("sub");
        std::fs::create_dir_all(&nested_real_sub).expect("mkdir nested_real/sub");
        let basis_path = nested_real_sub.join("data");
        std::fs::write(&basis_path, b"nested").expect("write");

        symlink("nested_real", tmp.path().join("nested")).expect("symlink");

        let through_link = tmp.path().join("nested").join("sub").join("data");
        let mut file = open_basis_nofollow(&through_link).expect("open through nested symlink");
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut file, &mut buf).expect("read");
        assert_eq!(buf, "nested");
    }

    /// Top-level basis (test 6 mirror): no dirname split needed.
    #[test]
    fn opens_top_level_basis_without_split() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let basis = tmp.path().join("topfile");
        {
            let mut f = std::fs::File::create(&basis).expect("create");
            f.write_all(b"top").expect("write");
        }
        let mut file = open_basis_nofollow(&basis).expect("open top-level");
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut file, &mut buf).expect("read");
        assert_eq!(buf, "top");
    }

    /// Missing path surfaces `ENOENT` so receiver fallback logic
    /// (reference dirs, fuzzy match) keeps working.
    #[test]
    fn missing_path_returns_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist");
        let err = open_basis_nofollow(&missing).expect_err("missing path must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
