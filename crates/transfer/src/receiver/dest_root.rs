//! Destination-root pre-flight for the receiver.
//!
//! Vets the destination operand the way upstream's get_local_name() does
//! (main.c:753-870) and creates the destination root directory when the
//! transfer needs one.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use logging::{ChangeDirFailed, ChangeDirStep};

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
    /// The destination directory exists but cannot be entered.
    /// upstream: main.c:776-781 - `change_dir#1 %s failed`.
    ChangeDir1,
    /// The destination directory this run creates could not be entered.
    /// upstream: main.c:836-840 - `change_dir#2 %s failed`.
    ChangeDir2,
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
        let step = match self.step {
            SelectStep::Stat => {
                let cause = logging::upstream_errno_text(&self.source);
                return write!(f, "ERROR: cannot stat destination {}: {cause}", self.fname);
            }
            SelectStep::ChangeDir1 => ChangeDirStep::One,
            SelectStep::ChangeDir2 => ChangeDirStep::Two,
            SelectStep::ChangeDir3 => ChangeDirStep::Three,
        };
        fmt::Display::fmt(&ChangeDirFailed::new(step, &self.fname, &self.source), f)
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
    /// The existing destination directory could not be entered.
    ChangeDir1(io::Error),
    /// The destination directory this run creates could not be entered, or
    /// its parent was refused before the mkdir.
    ChangeDir2(io::Error),
    /// The destination's parent could not be entered.
    ChangeDir3 { cause: io::Error, parent: PathBuf },
}

impl DestRootFailure {
    fn cause(&self) -> &io::Error {
        match self {
            Self::Stat(cause)
            | Self::Mkdir { cause, .. }
            | Self::ChangeDir1(cause)
            | Self::ChangeDir2(cause)
            | Self::ChangeDir3 { cause, .. } => cause,
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
    ensure_dest_root(
        dest_root,
        file_total,
        trailing_slash,
        dry_run,
        mkpath,
        Entry::Searched,
    )
    .map(|entered| entered.root == DestRoot::Created)
}

/// How the receiver enters the destination, standing in for upstream's
/// `change_dir()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::receiver) enum Entry {
    /// Through the ownership walk, which refuses a symlink owned by neither
    /// root nor the euid and yields the directory's descriptor.
    ///
    /// upstream: util1.c:1363-1392 change_dir() - the `!am_chrooted &&
    /// !am_sender && !insecure_links` arm, `open_no_attacker_symlinks_dirfd()`
    /// then `fchdir()`.
    Walked,
    /// A search-permission check on `<dir>/.`, which fails with the errno a
    /// plain `chdir()` would. oc never chdirs, so nothing is entered.
    ///
    /// upstream: util1.c:1393-1402 change_dir() - the plain `chdir()` arm
    /// (`--insecure-links`). A daemon module's destination is anchored by its
    /// own confined walk instead.
    Searched,
}

/// The walked descriptor of the directory the receiver entered.
#[cfg(unix)]
pub(in crate::receiver) type Anchor = std::os::fd::OwnedFd;

/// No platform but Unix walks the destination, so there is never an anchor.
#[cfg(not(unix))]
pub(in crate::receiver) type Anchor = std::convert::Infallible;

/// What [`ensure_dest_root`] found and entered.
#[derive(Debug)]
pub(in crate::receiver) struct EnteredRoot {
    /// Whether the root was present, created, or left absent by a dry run.
    pub(in crate::receiver) root: DestRoot,
    /// The walked descriptor of the directory entered, when one was walked.
    pub(in crate::receiver) anchor: Option<Anchor>,
    /// The get_local_name() step that entered it, which names a later failure
    /// to pin it.
    pub(in crate::receiver) step: SelectStep,
}

impl EnteredRoot {
    pub(in crate::receiver) const fn unentered(root: DestRoot) -> Self {
        Self {
            root,
            anchor: None,
            step: SelectStep::ChangeDir1,
        }
    }
}

/// Enters `dir` the way `entry` says.
///
/// upstream: util1.c:1385-1389 - after the walk, `fchdir()` needs search
/// permission, which the walk's `O_PATH` open does not; the stat of `<dir>/.`
/// checks it.
fn enter(dir: &Path, entry: Entry) -> io::Result<Option<Anchor>> {
    #[cfg(unix)]
    if entry == Entry::Walked {
        let anchor = fast_io::operator_open_destination(dir)?;
        searchable(dir)?;
        return Ok(Some(anchor));
    }
    #[cfg(not(unix))]
    let _ = entry;
    searchable(dir).map(|()| None)
}

fn searchable(dir: &Path) -> io::Result<()> {
    dir.join(".").metadata().map(drop)
}

/// Whether `error` is the ownership walk refusing a foreign-owned symlink.
///
/// The walk prints upstream's refusal line and fails with `ELOOP`; upstream
/// reports the same refusal from the `change_dir()` that follows its mkdir.
fn is_refusal(entry: Entry, error: &io::Error) -> bool {
    entry == Entry::Walked && fast_io::is_symlink_refusal(error)
}

/// Creates the destination root, refusing a foreign-owned symlink among its
/// leading directories before anything is created.
///
/// Deliberate deviation: upstream's `do_mkdir()` (main.c:817) follows such a
/// symlink, creates the directory wherever it points, and only the
/// `change_dir#2` that follows (main.c:836-840) refuses it. Here the parent is
/// reached through the ownership walk first, so nothing is left behind; the
/// refusal keeps upstream's `change_dir#2` line and exit code.
fn create_root(path: &Path, entry: Entry) -> io::Result<()> {
    const ACCESSPERMS: u32 = 0o777;
    #[cfg(unix)]
    if entry == Entry::Walked {
        return fast_io::operator_mkdir(path, ACCESSPERMS);
    }
    #[cfg(not(unix))]
    let _ = (entry, ACCESSPERMS);
    std::fs::create_dir(path)
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
    entry: Entry,
) -> io::Result<EnteredRoot> {
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
        made = make_path(dest_root, drop_name && !trailing_slash, dry_run, entry).map_err(
            |cause| {
                // upstream: main.c:757-781 - make_path() follows a foreign
                // symlink and change_dir#1 then refuses the directory it made.
                // An absolute chain is walked before anything is made, so the
                // refusal comes first and keeps that line; a relative one is
                // made beneath the cwd, whose ELOOP is make_path()'s own
                // failure (syscall.c:2306-2332).
                if is_refusal(entry, &cause) && dest_root.is_absolute() {
                    DestRootFailure::ChangeDir1(cause).into_io()
                } else {
                    DestRootFailure::Mkdir {
                        cause,
                        slash_kept: true,
                    }
                    .into_io()
                }
            },
        )?;
        if made > 0 {
            stat = dest_root.metadata();
        }
    }
    let created = made > 0 && !dry_run;
    match stat {
        Ok(meta) if meta.is_dir() => {
            // upstream: main.c:776-781 - change_dir#1 into the existing
            // directory, RERR_FILESELECT when it fails, even under --dry-run.
            let anchor = enter(dest_root, entry)
                .map_err(|cause| DestRootFailure::ChangeDir1(cause).into_io())?;
            return Ok(EnteredRoot {
                root: made_root(created),
                anchor,
                step: SelectStep::ChangeDir1,
            });
        }
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
            return Ok(EnteredRoot::unentered(DestRoot::DryRunAbsent));
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
        create_root(dest_root, entry).map_err(|cause| {
            if is_refusal(entry, &cause) {
                DestRootFailure::ChangeDir2(cause).into_io()
            } else {
                DestRootFailure::Mkdir {
                    cause,
                    slash_kept: false,
                }
                .into_io()
            }
        })?;
        // upstream: main.c:836-840 - change_dir#2 into the directory made.
        let anchor = enter(dest_root, entry)
            .map_err(|cause| DestRootFailure::ChangeDir2(cause).into_io())?;
        return Ok(EnteredRoot {
            root: DestRoot::Created,
            anchor,
            step: SelectStep::ChangeDir2,
        });
    }
    let Some(parent) = parent_of(dest_root) else {
        return Ok(EnteredRoot::unentered(made_root(created)));
    };
    // upstream: main.c:856-864 - a dry run under --mkpath does not enter the
    // parent it would have made.
    if dry_run && mkpath && parent.metadata().is_err() {
        return Ok(EnteredRoot::unentered(DestRoot::DryRunAbsent));
    }
    // upstream: main.c:865-869 - change_dir#3 into the lone file's parent.
    let anchor = enter(parent, entry).map_err(|cause| {
        DestRootFailure::ChangeDir3 {
            cause,
            parent: parent.to_path_buf(),
        }
        .into_io()
    })?;
    Ok(EnteredRoot {
        root: made_root(created),
        anchor,
        step: SelectStep::ChangeDir3,
    })
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
///
/// A walked entry counts the missing directories first and then builds the
/// chain through the ownership walk, so a foreign-owned symlink among the
/// existing ancestors is refused before anything is made (the deviation of
/// [`create_root`]). A relative chain takes upstream's own confined arm:
/// each component is made beneath the cwd, following in-tree symlinks and
/// failing with `ELOOP` on one that leaves (syscall.c:2306-2332 do_mkdir_at()).
fn make_path(path: &Path, drop_name: bool, dry_run: bool, entry: Entry) -> io::Result<usize> {
    let target = if drop_name {
        // upstream: util1.c:223-227 - no separator, or only the leading one.
        match parent_of(path) {
            Some(parent) if parent.parent().is_some() => parent,
            _ => return Ok(0),
        }
    } else {
        path
    };
    #[cfg(unix)]
    if entry == Entry::Walked && !dry_run {
        const ACCESSPERMS: u32 = 0o777;
        let missing = count_missing(target)?;
        if missing > 0 {
            if target.is_absolute() {
                fast_io::operator_make_path(target, ACCESSPERMS)?;
            } else {
                fast_io::make_path_beneath_cwd(target, ACCESSPERMS)?;
            }
        }
        return Ok(missing);
    }
    #[cfg(not(unix))]
    let _ = entry;
    count_or_make(target, if dry_run { Pass::DryRun } else { Pass::Make })
}

/// How many directories of `target` are missing, without making any.
#[cfg(unix)]
fn count_missing(target: &Path) -> io::Result<usize> {
    count_or_make(target, Pass::Count)
}

/// What [`count_or_make`] does with each missing directory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// upstream's `dry_run` arm (util1.c:233-239): stat only.
    DryRun,
    /// Count for a real run whose directories another walk then makes.
    #[cfg(unix)]
    Count,
    /// Make each one with `mkdir`.
    Make,
}

/// make_path()'s own loop: find the deepest existing directory of `target`,
/// then make each one below it, or only count them.
fn count_or_make(target: &Path, pass: Pass) -> io::Result<usize> {
    let mut missing = Vec::new();
    let mut made = 0;
    let mut dir = target;
    loop {
        let attempt = match pass {
            Pass::Make => std::fs::create_dir(dir),
            _ => match dir.metadata() {
                Ok(meta) if meta.is_dir() => break,
                // upstream: util1.c:234-239 - a dry run sets ENOTDIR for any
                // non-directory; this is the real ENOTDIR, from asking the
                // file for a directory entry.
                Ok(_) if pass == Pass::DryRun => Err(dir
                    .join(".")
                    .metadata()
                    .err()
                    .unwrap_or_else(|| io::Error::from(io::ErrorKind::NotADirectory))),
                // upstream: util1.c:241-246 - a real run's mkdir of an existing
                // non-directory fails with EEXIST. Only the deepest directory
                // can be one here: below a non-directory the stat already
                // failed with ENOTDIR. The walk that makes the chain issues
                // that mkdir and reports the kernel's errno.
                Ok(_) => {
                    missing.push(dir);
                    break;
                }
                // A non-directory above `dir`: the deepest mkdir is what fails,
                // and its errno depends on the walk (ELOOP for a relative path
                // whose non-directory sits above the parent, syscall.c:3279-3281),
                // so the making walk reports it rather than this stat.
                #[cfg(unix)]
                Err(err) if pass == Pass::Count && err.kind() == io::ErrorKind::NotADirectory => {
                    missing.push(dir);
                    break;
                }
                Err(err) => Err(err),
            },
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
        if pass == Pass::Make {
            std::fs::create_dir(dir)?;
        }
        made += 1;
    }
    Ok(made)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Entry, make_path};
    use std::fs;
    use std::path::Path;

    /// The error a plain mkdir of `deepest` gives: the errno 3.5.2's
    /// make_path() reports, since it tries the deepest directory first
    /// (util1.c:231-247).
    fn mkdir_errno(deepest: &Path) -> Option<i32> {
        fs::create_dir(deepest)
            .expect_err("mkdir must fail")
            .raw_os_error()
    }

    /// `dst/dstfile` is a regular file. Each case names the destination, whether
    /// make_path() drops its last name, and the deepest directory it tries.
    #[test]
    fn a_regular_file_in_the_chain_reports_the_deepest_mkdirs_errno() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).expect("mkdir dst");
        fs::write(dst.join("dstfile"), b"b\n").expect("write dstfile");
        // (destination, drop_name, deepest directory make_path() tries first)
        let cases = [
            ("dstfile/x", true, "dstfile"),
            ("dstfile/x/y", true, "dstfile/x"),
            ("dstfile/x", false, "dstfile/x"),
        ];
        for (dest, drop_name, deepest) in cases {
            let want = mkdir_errno(&dst.join(deepest));
            let error = make_path(&dst.join(dest), drop_name, false, Entry::Walked)
                .expect_err("a regular file blocks the chain");
            assert_eq!(
                error.raw_os_error(),
                want,
                "{dest} (drop_name={drop_name}): {error}"
            );
            assert!(
                dst.join("dstfile").is_file(),
                "{dest}: nothing replaced the file"
            );
        }
        // The two shapes the interop matrix separates: EEXIST where the file is
        // the deepest directory itself, ENOTDIR where it sits above it.
        assert_ne!(
            mkdir_errno(&dst.join("dstfile")),
            mkdir_errno(&dst.join("dstfile/x"))
        );
    }
}
