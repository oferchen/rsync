//! Destination-root pre-flight for the receiver.
//!
//! Vets the destination operand the way upstream's get_local_name() does
//! (main.c:753-870) and creates the destination root directory when the
//! transfer needs one.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

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

    /// Wraps the failure of `mkdir` on `fname` (already rendered by
    /// `full_fname()`) as an `io::Error` of the cause's kind.
    pub fn into_io(fname: String, source: io::Error) -> io::Error {
        io::Error::new(source.kind(), Self { fname, source })
    }
}

impl fmt::Display for DestRootMkdirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mkdir {} failed: {}",
            self.fname,
            logging::upstream_errno_text(&self.source)
        )
    }
}

impl std::error::Error for DestRootMkdirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The get_local_name() step a [`DestRootSelectError`] failed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectStep {
    /// The destination could not be stat()ed for a reason other than its
    /// absence. upstream: main.c:798-803 - `ERROR: cannot stat destination %s`.
    Stat,
    /// A lone non-directory's destination parent could not be entered.
    /// upstream: main.c:865-869 - `change_dir#3 %s failed`.
    ChangeDir3,
}

/// A destination get_local_name() refuses with `RERR_FILESELECT`.
///
/// upstream: main.c:798-803 and :865-869 - `rsyserr(FERROR, errno, ...)`, then
/// `exit_cleanup(RERR_FILESELECT)`. full_fname() keeps the operand's trailing
/// slash in the stat refusal: it is lopped only later, before the mkdir
/// (main.c:809-810).
#[derive(Debug)]
pub struct DestRootSelectError {
    step: SelectStep,
    fname: String,
    source: io::Error,
}

impl DestRootSelectError {
    /// The step that failed.
    #[must_use]
    pub const fn step(&self) -> SelectStep {
        self.step
    }

    /// The path as upstream's `full_fname()` renders it, quoted.
    #[must_use]
    pub fn fname(&self) -> &str {
        &self.fname
    }

    /// The error the step failed with.
    #[must_use]
    pub const fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Returns the refusal carried by `error`, if it is one.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// Wraps the failure of `step` on `fname` (already rendered by
    /// `full_fname()`) as an `io::Error` of the cause's kind.
    pub fn into_io(step: SelectStep, fname: String, source: io::Error) -> io::Error {
        io::Error::new(
            source.kind(),
            Self {
                step,
                fname,
                source,
            },
        )
    }
}

impl fmt::Display for DestRootSelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cause = logging::upstream_errno_text(&self.source);
        match self.step {
            SelectStep::Stat => write!(f, "ERROR: cannot stat destination {}: {cause}", self.fname),
            SelectStep::ChangeDir3 => write!(f, "change_dir#3 {} failed: {cause}", self.fname),
        }
    }
}

impl std::error::Error for DestRootSelectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Which step of the destination-root pre-flight refused, with its cause.
///
/// Carried inside the `io::Error` that [`ensure_dest_root_exists`] returns, so
/// the caller can render each with upstream's `full_fname()`.
#[derive(Debug)]
pub(in crate::receiver) enum DestRootFailure {
    /// `stat` failed for a reason other than the destination's absence.
    Stat(io::Error),
    /// `mkdir` failed. `slash_kept` is true for the make_path() failure under
    /// `--mkpath`, which upstream reports before it lops the trailing slash
    /// (main.c:757-762 vs :809-821).
    Mkdir { cause: io::Error, slash_kept: bool },
    /// The destination's parent could not be entered.
    ChangeDir3 { cause: io::Error, parent: PathBuf },
}

impl DestRootFailure {
    fn cause(&self) -> &io::Error {
        match self {
            Self::Stat(cause) | Self::Mkdir { cause, .. } | Self::ChangeDir3 { cause, .. } => cause,
        }
    }

    fn into_io(self) -> io::Error {
        io::Error::new(self.cause().kind(), self)
    }
}

impl fmt::Display for DestRootFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.cause().fmt(f)
    }
}

impl std::error::Error for DestRootFailure {}

/// Splits a [`ensure_dest_root_exists`] failure into the refused step, or
/// returns the error unchanged when it is not one.
pub(in crate::receiver) fn dest_root_failure(
    error: io::Error,
) -> Result<DestRootFailure, io::Error> {
    if !error
        .get_ref()
        .is_some_and(|inner| inner.is::<DestRootFailure>())
    {
        return Err(error);
    }
    let kind = error.kind();
    match error
        .into_inner()
        .map(|inner| inner.downcast::<DestRootFailure>())
    {
        Some(Ok(failure)) => Ok(*failure),
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

/// Vets the destination operand and creates the destination root directory
/// when the transfer needs one.
///
/// Mirrors upstream `get_local_name()` (`main.c:753-870`) in its order:
///
/// 1. stat the operand (`main.c:753`);
/// 2. under `--mkpath`, make_path() the missing chain first, so its failure is
///    `mkdir %s failed` even where the stat would also have refused
///    (`main.c:757-773`);
/// 3. accept an existing directory (`main.c:775-785`);
/// 4. refuse a stat failure other than `ENOENT` (`main.c:798-803`);
/// 5. for more than one entry or a trailing slash, mkdir the root
///    (`main.c:808-822`);
/// 6. otherwise the operand names a lone file: enter its parent, refusing one
///    that cannot be entered (`main.c:845-870`).
///
/// A dry run takes every step but the mkdirs, as upstream's `do_mkdir()` and
/// make_path() do nothing under `dry_run` (syscall.c:2256, util1.c:233).
///
/// `dest_root` is the operand as given, before a lone file's name is split off
/// it. Returns `Ok(true)` when a directory was created, `Ok(false)` when the
/// pre-flight created nothing.
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
    ensure_dest_root(dest_root, file_total, trailing_slash, dry_run, mkpath)
        .map(|root| root == DestRoot::Created)
}

/// What [`ensure_dest_root`] found at the destination root.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::receiver) enum DestRoot {
    /// The root existed, or names a single file's destination.
    #[default]
    Present,
    /// The pre-flight mkdir created the root (`FLAG_DIR_CREATED`).
    Created,
    /// A dry run would have created the root or its `--mkpath` parent, so it
    /// is not there: upstream bumps `dry_run` past 1 (main.c:833, :863).
    DryRunAbsent,
}

/// [`ensure_dest_root_exists`], also telling a root a dry run left absent
/// from one that is present.
pub(in crate::receiver) fn ensure_dest_root(
    dest_root: &Path,
    file_total: usize,
    trailing_slash: bool,
    dry_run: bool,
    mkpath: bool,
) -> io::Result<DestRoot> {
    let made_root = |created: bool| {
        if created {
            DestRoot::Created
        } else {
            DestRoot::Present
        }
    };
    let mut stat = dest_root.metadata();
    // upstream: main.c:754 - `cp = strrchr(dest_path, '/')`.
    let has_separator = trailing_slash || parent_of(dest_root).is_some();
    let mut made = 0;
    if mkpath && stat.is_err() && (has_separator || file_total > 1) {
        let drop_name = file_total <= 1 || trailing_slash;
        made = make_path(dest_root, drop_name && !trailing_slash, dry_run).map_err(|cause| {
            DestRootFailure::Mkdir {
                cause,
                slash_kept: true,
            }
            .into_io()
        })?;
        if made > 0 {
            stat = dest_root.metadata();
        }
    }
    let created = made > 0 && !dry_run;
    match stat {
        Ok(meta) if meta.is_dir() => return Ok(made_root(created)),
        Ok(_) if file_total > 1 || trailing_slash => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "destination root '{}' exists but is not a directory",
                    dest_root.display(),
                ),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() != io::ErrorKind::NotFound => {
            return Err(DestRootFailure::Stat(err).into_io());
        }
        Err(_) => {}
    }
    if file_total > 1 || trailing_slash {
        if dry_run {
            return Ok(DestRoot::DryRunAbsent);
        }
        // Distinguish "path absent" from "dangling symlink": if lstat
        // shows a symlink while stat says NotFound, the link target is
        // missing and the mkdir would resolve the link and materialize a
        // directory at the target. Refuse that footgun by surfacing the
        // original stat error.
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
        // upstream: main.c:817 - a single do_mkdir(); an ancestor that is
        // missing fails it with ENOENT unless --mkpath made it above.
        return std::fs::create_dir(dest_root)
            .map(|()| DestRoot::Created)
            .map_err(|cause| {
                DestRootFailure::Mkdir {
                    cause,
                    slash_kept: false,
                }
                .into_io()
            });
    }
    let Some(parent) = parent_of(dest_root) else {
        return Ok(made_root(created));
    };
    // upstream: main.c:856-864 - a dry run under --mkpath does not enter the
    // parent it would have made.
    if dry_run && mkpath && parent.metadata().is_err() {
        return Ok(DestRoot::DryRunAbsent);
    }
    // upstream: main.c:865 change_dir(dest_path, CD_NORMAL). Stat-ing
    // `<parent>/.` needs what the chdir needs - a directory the process may
    // search - and fails with the same errno, without moving the process.
    parent.join(".").metadata().map_err(|cause| {
        DestRootFailure::ChangeDir3 {
            cause,
            parent: parent.to_path_buf(),
        }
        .into_io()
    })?;
    Ok(made_root(created))
}

/// The directory part of `path`, or `None` when it has none.
///
/// upstream: main.c:852-853 - a leading `/` alone leaves the root, `"/"`.
fn parent_of(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Creates every missing directory of `path` (of its parent when `drop_name`)
/// and returns how many it created, or would create under `dry_run`.
///
/// upstream: util1.c:210-290 make_path() - find the deepest existing
/// directory, then make each one below it. An ancestor that exists but is not
/// a directory fails with that step's errno.
fn make_path(path: &Path, drop_name: bool, dry_run: bool) -> io::Result<usize> {
    let target = if drop_name {
        // upstream: util1.c:223-227 - no separator, or only the leading one.
        match parent_of(path) {
            Some(parent) if parent.parent().is_some() => parent,
            _ => return Ok(0),
        }
    } else {
        path
    };
    let mut missing = Vec::new();
    let mut made = 0;
    let mut dir = target;
    loop {
        let attempt = if dry_run {
            match dir.metadata() {
                Ok(meta) if meta.is_dir() => break,
                // The real ENOTDIR, from asking the file for a directory entry.
                Ok(_) => Err(dir
                    .join(".")
                    .metadata()
                    .err()
                    .unwrap_or_else(|| io::Error::from(io::ErrorKind::NotADirectory))),
                Err(err) => Err(err),
            }
        } else {
            std::fs::create_dir(dir)
        };
        match attempt {
            Ok(()) => {
                made += 1;
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err)
                if err.kind() == io::ErrorKind::AlreadyExists
                    && dir.metadata().is_ok_and(|m| m.is_dir()) =>
            {
                break;
            }
            Err(err) => return Err(err),
        }
        missing.push(dir);
        // upstream: util1.c:252-266 - a relative path with nothing existing
        // assumes `.`, and an absolute one assumes `/`.
        match parent_of(dir) {
            Some(parent) if parent.parent().is_some() => dir = parent,
            _ => break,
        }
    }
    for dir in missing.into_iter().rev() {
        if !dry_run {
            std::fs::create_dir(dir)?;
        }
        made += 1;
    }
    Ok(made)
}
