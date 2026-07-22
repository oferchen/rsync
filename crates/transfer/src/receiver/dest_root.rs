//! Destination-root pre-flight helpers for the receiver.
//!
//! Detects a trailing path separator on the destination operand and creates
//! the destination root directory when the transfer needs one.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

/// Reports whether a destination operand was written with a trailing path
/// separator.
///
/// Upstream rsync inspects the raw `dest_path` argument (`main.c:733-734`)
/// after a final `strrchr('/')` to decide whether the operand ends with a
/// directory marker. The detection is byte-level on Unix and matches either
/// `'/'` or `'\\'` on Windows so paths produced by either separator convention
/// are honored.
pub(in crate::receiver) fn dest_arg_has_trailing_slash(arg: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        arg.as_bytes().last() == Some(&b'/')
    }
    #[cfg(windows)]
    {
        let bytes = arg.as_encoded_bytes();
        matches!(bytes.last(), Some(b'/') | Some(b'\\'))
    }
    #[cfg(not(any(unix, windows)))]
    {
        arg.to_string_lossy().ends_with('/')
    }
}

/// Creates the destination root directory when the transfer needs one.
///
/// Mirrors upstream `main.c:778-792` (`get_local_name()`): when the receiver
/// is about to write more than one file, or the destination operand carries a
/// trailing slash, the root must exist as a directory before per-entry mkdir
/// dispatch. The local-mode receiver gets this for free via the file-list-
/// driven implicit mkdir, but the `--server` path never created the root, so
/// the alt-dest upstream interop test that runs over remote-shell failed when
/// the destination did not already exist.
///
/// Returns `Ok(true)` when a new directory was created, `Ok(false)` when the
/// pre-flight was a no-op (already exists, single-file transfer without
/// trailing slash, or `dry_run`).
///
/// # Symlink resolution
///
/// The existence check uses `metadata()` (stat) rather than
/// `symlink_metadata()` (lstat) so a symlink at `dest_root` pointing at a
/// real directory is accepted, matching upstream `main.c:745-754`
/// `get_local_name()` which calls `do_stat()` (follows symlinks) and
/// proceeds when `S_ISDIR(st.st_mode)` is true. A symlinked dest root is
/// the upstream `symlink-dirlink-basis` interop scenario (issue #715).
///
/// A dangling symlink resolves to `ENOENT`, and the helper would then create
/// the directory, which would materialize it at the symlink target. To avoid
/// that footgun we lstat first when the stat reports `NotFound`: if the path
/// is actually a dangling symlink we propagate the stat-side `NotFound`
/// instead of auto-creating through the link.
///
/// # `--mkpath` gating
///
/// Upstream creates the destination root with a *single* `do_mkdir(dest_path)`
/// (`main.c:797-801`), which fails with `ENOENT` when an ancestor directory is
/// missing. The whole missing chain is created only under `--mkpath`, via
/// `make_path(dest_path)` (`main.c:736`). This helper mirrors that: with
/// `mkpath == false` it uses [`std::fs::create_dir`] (single level), with
/// `mkpath == true` it uses [`std::fs::create_dir_all`] (full chain). Passing
/// `create_dir_all` unconditionally would auto-create deep destination paths
/// without `--mkpath`, diverging from upstream.
///
/// # Security model
///
/// Accepting a symlinked dest is safe because per-entry writes are sandboxed
/// downstream of this helper:
///
/// - SEC-1.e/.f-.j: every per-entry open routes through a [`DirSandbox`]
///   anchored at the resolved canonical path (see `transfer::setup`), and
///   each path-component open uses `openat2(RESOLVE_BENEATH |
///   RESOLVE_NO_SYMLINKS)` on Linux 5.6+ or `openat(O_NOFOLLOW |
///   O_DIRECTORY)` elsewhere. A malicious symlink at `dest_root` resolves
///   once here, then the sandbox locks every subsequent open below that
///   resolved fd.
/// - The daemon module-containment check is performed by the daemon module
///   loader, not by this helper. A symlinked dest cannot escape the module
///   root because the daemon already chroots / restricts `module.path` at
///   the module-config layer.
///
/// [`DirSandbox`]: fast_io::DirSandbox
///
/// # Upstream Reference
///
/// - `main.c:745-754` - `get_local_name()` `S_ISDIR(st.st_mode)` branch:
///   `do_stat()` follows symlinks, `change_dir()` resolves the link and
///   enters the target.
/// - `main.c:736` - `get_local_name()` `--mkpath` branch:
///   `make_path(dest_path, ...)` creates the whole missing chain.
/// - `main.c:778-792` - `get_local_name()` pre-flight `do_mkdir(dest_path, ACCESSPERMS)`
///   (single level, no ancestor creation) when `--mkpath` was not requested.
/// - `main.c:803-805` - sets `FLAG_DIR_CREATED` on the first flist entry when
///   its basename is `.` (deferred follow-up; oc-rsync's delete path does
///   not currently consume that flag).
pub fn ensure_dest_root_exists(
    dest_root: &Path,
    file_total: usize,
    trailing_slash: bool,
    dry_run: bool,
    mkpath: bool,
) -> io::Result<bool> {
    if dry_run {
        return Ok(false);
    }
    // upstream: main.c:797 - the non-mkpath dir branch only fires for a
    // multi-file transfer or a trailing-slash operand. A single-file
    // no-slash operand names the destination file itself (the operand
    // basename was already split off by `apply_single_file_rename`), so
    // without `--mkpath` there is no root to pre-create here.
    //
    // upstream: main.c:736 - under `--mkpath`, `make_path(dest_path, ...)`
    // also creates the parent chain of a single-file operand (the
    // `MKP_DROP_NAME` case), so the early no-op must not swallow the mkpath
    // path. `dest_root` here is already the parent directory for the
    // single-file rename, so creating it materializes the missing chain.
    if file_total <= 1 && !trailing_slash && !mkpath {
        return Ok(false);
    }
    // stat (follows symlinks) so a symlinked dest pointing at a real
    // directory is accepted, matching upstream main.c:745-754 do_stat() +
    // S_ISDIR + change_dir() flow. A non-directory target (regular file,
    // broken symlink, etc.) is still rejected at the call site below.
    match dest_root.metadata() {
        Ok(meta) if meta.is_dir() => Ok(false),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "destination root '{}' exists but is not a directory",
                dest_root.display(),
            ),
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // Distinguish "path absent" from "dangling symlink": if lstat
            // shows a symlink while stat says NotFound, the link target is
            // missing and create_dir_all would resolve the link and
            // materialize a directory at the target. Refuse that footgun
            // by surfacing the original stat error.
            if dest_root
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "destination root '{}' is a dangling symlink",
                        dest_root.display(),
                    ),
                ));
            }
            // upstream: main.c:736 make_path (full chain) only under --mkpath;
            // otherwise main.c:797 do_mkdir creates a single level and fails
            // with ENOENT when an ancestor is missing.
            if mkpath {
                std::fs::create_dir_all(dest_root).map(|()| true)
            } else {
                std::fs::create_dir(dest_root).map(|()| true)
            }
        }
        Err(err) => Err(err),
    }
}
