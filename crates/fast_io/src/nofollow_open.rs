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
//! # Regular files only
//!
//! Upstream never reaches its basis open (`generator.c:2313`,
//! `do_open_checklinks(fnamecmp)`) with a non-regular `fnamecmp`.
//! `generator.c:2148` has already removed a non-regular destination
//! (`delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_FILE)`, leaving
//! `statret = -1`); `try_dests_reg()` accepts an alt-dest candidate only
//! when `S_ISREG(sxp->st.st_mode)` (`generator.c:1084`); `find_fuzzy()`
//! skips anything that is not `S_ISREG(fp->mode)` (`generator.c:860,888`);
//! and the `--partial-dir` fallback is guarded by
//! `S_ISREG(partial_st.st_mode)` (`generator.c:2175`). Every basis upstream
//! opens is therefore a regular file, and this helper states that invariant
//! directly.
//!
//! Stating it matters because opening an unchecked node blocks: a FIFO with
//! no writer parks the opener in `fifo_open()` indefinitely, and a device
//! can block on carrier. The check is made on the descriptor rather than
//! with a pre-open `fstatat(2)`, so a node swapped between check and open
//! cannot defeat it: the open carries `O_NONBLOCK` (upstream does the same
//! for an unchecked leaf at `generator.c:1024`,
//! `O_RDONLY | O_NOFOLLOW | O_NONBLOCK`) so it can never park, `fstat(2)`
//! then decides, and `O_NONBLOCK` is cleared before a regular-file
//! descriptor is handed back so the basis read behaves exactly as before.
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
/// - [`io::ErrorKind::InvalidInput`] when the opened node is not a regular
///   file (FIFO, socket, device, directory). Callers treat that as "no
///   basis", which is the outcome upstream reaches by having already
///   removed the obstacle at `generator.c:2148`.
/// - Any other I/O error from opening the parent directory or the
///   basename (forwarded verbatim from the underlying syscall).
pub fn open_basis_nofollow(path: &Path) -> io::Result<File> {
    let file = imp::open_basis_nofollow(path)?;
    require_regular_basis(file)
}

/// Refuses a non-regular basis and hands a regular one back with
/// `O_NONBLOCK` cleared.
///
/// The descriptor is already open here, so the decision is made with
/// `fstat(2)` on the descriptor itself: a pre-open `fstatat(2)` would leave
/// a window in which the node it checked is replaced by the FIFO the check
/// exists to refuse.
fn require_regular_basis(file: File) -> io::Result<File> {
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "delta basis is not a regular file",
        ));
    }
    imp::clear_nonblock(&file)?;
    Ok(file)
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
            return open_nonblock(path);
        };

        // Upstream `syscall.c:727`: `if (!slash) return do_open(...)`.
        // Treat the empty parent ("foo" with no slash) and the lone
        // root component identically: there is no dirname to split.
        let dirname = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => return open_nonblock(path),
        };

        let dir = open_dir_follow(dirname)?;
        // `O_NONBLOCK` keeps the open itself from parking on a FIFO with no
        // writer or a device without carrier; the node type is then decided
        // by `fstat(2)` in `require_regular_basis`. upstream does the same
        // for an unchecked leaf at `generator.c:1024`.
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        openat(dir.as_fd(), basename, flags, 0)
    }

    /// The `if (!slash)` short-circuit's open. Carries `O_NONBLOCK` for the
    /// same reason the `openat(2)` above does: the node type is not known
    /// until the descriptor exists.
    fn open_nonblock(path: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    }

    /// Drops `O_NONBLOCK` from an already-typed regular-file descriptor so
    /// the basis read sees the same file status flags it saw before.
    pub(super) fn clear_nonblock(file: &File) -> io::Result<()> {
        let flags = rustix::fs::fcntl_getfl(file)?;
        if flags.contains(rustix::fs::OFlags::NONBLOCK) {
            rustix::fs::fcntl_setfl(file, flags - rustix::fs::OFlags::NONBLOCK)?;
        }
        Ok(())
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

    /// This platform's open sets no `O_NONBLOCK`, so there is nothing to
    /// clear. The regular-file check in `require_regular_basis` still
    /// applies.
    pub(super) fn clear_nonblock(_file: &File) -> io::Result<()> {
        Ok(())
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

    /// A FIFO with no writer must be refused, and refused *promptly*: an
    /// `open(O_RDONLY)` without `O_NONBLOCK` parks in `fifo_open()` until a
    /// writer arrives, which never happens for a planted destination node.
    ///
    /// The bound is a deadline on a worker thread, not a sleep: before the
    /// fix this test reports a timeout instead of wedging the run.
    #[cfg(unix)]
    #[test]
    fn refuses_fifo_basis_without_blocking() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fifo = tmp.path().join("fifo-basis");
        // Same `mkfifo(1)` shell-out as tests/drop_devices.rs.
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("spawn mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(open_basis_nofollow(&fifo).map(|_| ()));
        });

        let outcome = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("basis open on a FIFO must return, not block on a writer");
        let err = outcome.expect_err("a FIFO must not be accepted as a delta basis");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A directory opens successfully with `O_RDONLY` but can never be read
    /// as a delta basis. Upstream removes it at `generator.c:2148`; oc
    /// declines it here.
    #[test]
    fn refuses_directory_basis() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("dir-basis");
        std::fs::create_dir(&dir).expect("mkdir");
        let err = open_basis_nofollow(&dir).expect_err("a directory is not a delta basis");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// `O_NONBLOCK` exists only to keep the open from parking. A regular
    /// basis must be handed back with the same file status flags it had
    /// before, so the delta read is byte-for-byte the previous behaviour.
    #[cfg(unix)]
    #[test]
    fn regular_basis_descriptor_has_no_nonblock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let basis = tmp.path().join("sub").join("basis");
        std::fs::create_dir(basis.parent().expect("parent")).expect("mkdir");
        std::fs::write(&basis, b"payload").expect("write basis");

        let file = open_basis_nofollow(&basis).expect("regular basis must open");
        let flags = rustix::fs::fcntl_getfl(&file).expect("fcntl F_GETFL");
        assert!(
            !flags.contains(rustix::fs::OFlags::NONBLOCK),
            "O_NONBLOCK must be cleared on a regular basis descriptor: {flags:?}"
        );
    }
}
