//! Destination-root pre-flight helpers for the receiver.
//!
//! Detects a trailing path separator on the destination operand and creates
//! the destination root directory when the transfer needs one.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::Path;

/// The failed pre-flight `mkdir` of the destination root.
///
/// upstream: main.c:817-821 get_local_name() - `rsyserr(FERROR, errno,
/// "mkdir %s failed", full_fname(dest_path))`, then `exit_cleanup(RERR_FILEIO)`.
/// The make_path() failure under `--mkpath` takes the same `mkdir_error` label.
#[derive(Debug)]
pub struct DestRootMkdirError {
    fname: String,
    source: io::Error,
}

impl DestRootMkdirError {
    /// The destination root as upstream's `full_fname()` renders it, quoted.
    #[must_use]
    pub fn fname(&self) -> &str {
        &self.fname
    }

    /// The error `mkdir` failed with.
    #[must_use]
    pub const fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Returns the mkdir failure carried by `error`, if it is one.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// Wraps the failure as an `io::Error` of the cause's kind.
    pub(in crate::receiver) fn into_io(fname: String, source: io::Error) -> io::Error {
        io::Error::new(source.kind(), Self { fname, source })
    }
}

impl fmt::Display for DestRootMkdirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mkdir {} failed: {}", self.fname, self.source)
    }
}

impl std::error::Error for DestRootMkdirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Marks the `mkdir` step's own failure so the caller can tell it from the
/// stat-side refusals around it.
#[derive(Debug)]
struct MkdirFailed(io::Error);

impl fmt::Display for MkdirFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for MkdirFailed {}

/// Splits a [`ensure_dest_root_exists`] failure into the `mkdir` cause, when
/// that is what failed, or the error unchanged.
pub(in crate::receiver) fn mkdir_cause(error: io::Error) -> Result<io::Error, io::Error> {
    if !error
        .get_ref()
        .is_some_and(|inner| inner.is::<MkdirFailed>())
    {
        return Err(error);
    }
    let kind = error.kind();
    match error
        .into_inner()
        .map(|inner| inner.downcast::<MkdirFailed>())
    {
        Some(Ok(failed)) => Ok(failed.0),
        Some(Err(other)) => Err(io::Error::new(kind, other)),
        None => Err(io::Error::from(kind)),
    }
}

/// Reports whether a destination operand was written with a trailing path
/// separator.
///
/// Upstream rsync inspects the raw `dest_path` argument (`main.c:754-755`)
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
/// Mirrors upstream `main.c:808-822` (`get_local_name()`): when the receiver
/// is about to write more than one file, or the destination operand carries a
/// trailing slash, the root must exist as a directory before per-entry mkdir
/// dispatch. The local-mode receiver gets this from the file-list-driven
/// implicit mkdir; the `--server` receiver relies on this helper.
///
/// Returns `Ok(true)` when a new directory was created, `Ok(false)` when the
/// pre-flight was a no-op (already exists, single-file transfer without
/// trailing slash, or `dry_run`).
///
/// # Symlink resolution
///
/// The existence check uses `metadata()` (stat) rather than
/// `symlink_metadata()` (lstat) so a symlink at `dest_root` pointing at a
/// real directory is accepted, matching upstream `main.c:775-785`
/// `get_local_name()` which calls `do_stat()` (follows symlinks) and
/// proceeds when `S_ISDIR(st.st_mode)` is true. A symlinked dest root is
/// the upstream `symlink-dirlink-basis` interop scenario.
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
/// (`main.c:817`), which fails with `ENOENT` when an ancestor directory is
/// missing. The whole missing chain is created only under `--mkpath`, via
/// `make_path(dest_path)` (`main.c:757-773`). This helper mirrors that: with
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
/// - Every per-entry open routes through a [`DirSandbox`]
///   anchored at the resolved canonical path (see `transfer::setup`), and
///   each path-component open uses `openat2(RESOLVE_BENEATH |
///   RESOLVE_NO_SYMLINKS)` on Linux 5.6+ or `openat(O_NOFOLLOW |
///   O_DIRECTORY)` elsewhere. A malicious symlink at `dest_root` resolves
///   once here, then the sandbox locks every subsequent open below that
///   resolved fd.
/// - The daemon module-containment check is performed by the daemon module
///   loader, not by this helper.
///
/// ⚠ Containment alone is *not* sufficient on a `path = /` module: upstream's
/// `abspath_outside_confinement` short-circuits when the confinement root is
/// `/` (`rootlen <= 1`, `syscall.c:254`), so nothing about `module.path`
/// constrains a peer-supplied `..` traversal there. What upstream relies on in
/// that configuration is the module's NAME-based `exclude`, applied to the
/// destination argument in `get_local_name()` (`main.c:731-750`). oc mirrors
/// that in `ReceiverContext::reject_daemon_excluded_destination`, which runs
/// before this helper.
///
/// [`DirSandbox`]: fast_io::DirSandbox
///
/// # Upstream Reference
///
/// - `main.c:775-785` - `get_local_name()` `S_ISDIR(st.st_mode)` branch:
///   `do_stat()` follows symlinks, `change_dir()` resolves the link and
///   enters the target.
/// - `main.c:757-773` - `get_local_name()` `--mkpath` branch:
///   `make_path(dest_path, ...)` creates the whole missing chain.
/// - `main.c:817` - `get_local_name()` pre-flight `do_mkdir(dest_path, ACCESSPERMS)`
///   (single level, no ancestor creation) when `--mkpath` was not requested.
/// - `main.c:824-826` - sets `FLAG_DIR_CREATED` on the first flist entry when
///   its basename is `.`. Deviation from upstream: this helper does not set
///   that flag, and oc's delete path does not consume it.
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
    // upstream: main.c:808 - the non-mkpath dir branch only fires for a
    // multi-file transfer or a trailing-slash operand. A single-file
    // no-slash operand names the destination file itself (the operand
    // basename was already split off by `apply_single_file_rename`), so
    // without `--mkpath` there is no root to pre-create here.
    //
    // upstream: main.c:759 - under `--mkpath`, `make_path(dest_path, ...)`
    // also creates the parent chain of a single-file operand (the
    // `MKP_DROP_NAME` case), so the early no-op must not swallow the mkpath
    // path. `dest_root` here is already the parent directory for the
    // single-file rename, so creating it materializes the missing chain.
    if file_total <= 1 && !trailing_slash && !mkpath {
        return Ok(false);
    }
    // stat (follows symlinks) so a symlinked dest pointing at a real
    // directory is accepted, matching upstream main.c:753 do_stat() +
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
            // upstream: main.c:759 make_path (full chain) only under --mkpath;
            // otherwise main.c:817 do_mkdir creates a single level and fails
            // with ENOENT when an ancestor is missing.
            let created = if mkpath {
                std::fs::create_dir_all(dest_root)
            } else {
                std::fs::create_dir(dest_root)
            };
            created
                .map(|()| true)
                .map_err(|cause| io::Error::new(cause.kind(), MkdirFailed(cause)))
        }
        Err(err) => Err(err),
    }
}
