//! Destination state queries and target path computation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use protocol::iconv::FilenameConverter;

use crate::local_copy::{
    DestinationEntry, LocalCopyArgumentError, LocalCopyError, LocalCopyExecution,
};

use super::super::follow_symlink_metadata;
use super::super::transcode_filename_component;
use super::types::DestinationState;

/// Queries the filesystem to determine destination state.
///
/// Under `--mkpath` a failed stat is not reported: upstream hands the path to
/// make_path(), whose own mkdir errno names the failure (main.c:757-761,
/// :818-821). Without it only ENOENT means "absent".
pub(crate) fn query_destination_state(
    path: &Path,
    mkpath: bool,
) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            let symlink_to_dir = if file_type.is_symlink() {
                follow_symlink_metadata(path)
                    .map(|target| target.file_type().is_dir())
                    .unwrap_or(false)
            } else {
                false
            };

            Ok(DestinationState {
                exists: true,
                is_dir: file_type.is_dir(),
                symlink_to_dir,
            })
        }
        Err(error) if mkpath || error.kind() == io::ErrorKind::NotFound => {
            Ok(DestinationState::default())
        }
        Err(error) => Err(LocalCopyError::io(
            "inspect destination",
            path.to_path_buf(),
            error,
        )),
    }
}

/// Enters a destination directory before anything is transferred, as
/// upstream's `change_dir()` does, so later opens resolve from it.
///
/// On Unix the directory is reached through the ownership walk and pinned.
/// Every later open beneath it starts from the pinned descriptor, as upstream
/// works from the directory it `fchdir()`ed into, so a component swapped for a
/// symlink after this point cannot redirect the transfer. `--insecure-links`
/// takes upstream's plain `chdir()` arm, which needs only search permission.
///
/// upstream: util1.c:1368-1392 and :1450-1462 change_dir() -
/// `open_no_attacker_symlinks_dirfd()` for a non-daemon receiver; main.c
/// get_local_name() names the failure `change_dir#N` and exits
/// RERR_FILESELECT.
pub(crate) fn enter_destination(
    directory: &Path,
    entry: DestinationEntry,
) -> Result<(), LocalCopyError> {
    pin_entered(directory)
        .map_err(|error| LocalCopyError::destination_entry_failed(entry, directory, error))
}

#[cfg(unix)]
fn pin_entered(directory: &Path) -> io::Result<()> {
    if fast_io::confinement::session_optout_allowed() {
        return searchable(directory);
    }
    fast_io::operator_open_destination(directory)
        .and_then(|anchor| fast_io::confinement::pin_destination_root(directory, anchor))?;
    // upstream: util1.c:1462 - the walk is followed by fchdir(), which needs
    // search permission the walk's O_PATH open does not.
    searchable(directory)
}

#[cfg(not(unix))]
fn pin_entered(directory: &Path) -> io::Result<()> {
    searchable(directory)
}

/// `chdir()` needs search permission on the directory; so does a stat of
/// `<directory>/.`.
fn searchable(directory: &Path) -> io::Result<()> {
    match fs::metadata(directory.join(".")) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::from(io::ErrorKind::NotADirectory)),
        Err(error) => Err(error),
    }
}

/// Ensures the destination path is a directory, creating it if necessary.
///
/// Returns `true` when this call materialised the destination directory,
/// `false` when it was already present. upstream: main.c:811-812 - the
/// generator emits `created directory %s` only when the pre-flight mkdir
/// actually created the dest; subsequent runs against the same destination
/// must remain silent.
///
/// The `mkpath` argument selects how many components may be created, mirroring
/// upstream `get_local_name()`: without `--mkpath` a single `do_mkdir(dest_path)`
/// creates only the final component and a missing ancestor is a fatal ENOENT
/// (`main.c:800,809`), whereas `--mkpath` first runs `make_path(dest_path, ...)`
/// to build the whole leading chain (`main.c:749`).
pub(crate) fn ensure_destination_directory(
    destination_path: &Path,
    state: &mut DestinationState,
    mode: LocalCopyExecution,
    mkpath: bool,
) -> Result<bool, LocalCopyError> {
    if state.exists {
        if !state.is_dir {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::DestinationMustBeDirectory,
            ));
        }
        return Ok(false);
    }

    if mode.is_dry_run() {
        state.exists = true;
        state.is_dir = true;
        return Ok(true);
    }

    create_destination(destination_path, mkpath)
        .map_err(|error| creation_error(destination_path, mkpath, error))?;
    state.exists = true;
    state.is_dir = true;
    Ok(true)
}

/// Builds the missing leading directories of a single-entry destination under
/// `--mkpath`, through the ownership walk like the destination itself.
///
/// upstream: main.c:757-760 - make_path(dest_path, MKP_DROP_NAME); a failure
/// reports `mkdir %s failed` for the whole destination (main.c:819) and exits
/// RERR_FILEIO. A refusal of a foreign-owned symlink is reported as the
/// `change_dir#3` upstream prints after its make_path() followed the link.
pub(crate) fn make_single_entry_parent(
    destination: &Path,
    parent: &Path,
) -> Result<(), LocalCopyError> {
    create_destination(parent, true).map_err(|error| {
        if is_symlink_refusal(parent, true, &error) {
            LocalCopyError::destination_entry_failed(DestinationEntry::Parent, parent, error)
        } else {
            LocalCopyError::destination_mkdir_failed(destination, error)
        }
    })
}

/// The entry that enters a destination this run created: `--mkpath` re-stats
/// what make_path() built and enters it as an existing directory.
///
/// upstream: main.c:757-781 (`change_dir#1`) and main.c:817-839
/// (`change_dir#2`).
pub(crate) const fn created_destination_entry(mkpath: bool) -> DestinationEntry {
    if mkpath {
        DestinationEntry::Existing
    } else {
        DestinationEntry::Created
    }
}

/// Creates the destination directory, its leading directories too under
/// `--mkpath`.
///
/// upstream: main.c:759 vs main.c:817 - `--mkpath` runs `make_path()` (full
/// chain) while the plain path does a single `do_mkdir(dest_path)` that fails
/// with ENOENT when a leading directory is absent. Both modes are
/// `ACCESSPERMS` under the umask.
///
/// Deliberate deviation: upstream's `do_mkdir()` follows a symlink in the
/// leading directories, creates the directory wherever it points, and only
/// the `change_dir()` that follows refuses it. Here the leading directories
/// are resolved through the ownership walk before anything is created, so a
/// foreign-owned symlink is refused first and nothing is left behind.
/// `--insecure-links` keeps the plain, symlink-following creation.
///
/// A relative `--mkpath` destination takes upstream's own confined arm
/// instead: each component is created beneath the cwd through the walk that
/// follows in-tree symlinks and fails with `ELOOP` on one that leaves the
/// tree (syscall.c:2306-2332 `do_mkdir_at()`, util1.c:210-280 `make_path()`).
#[cfg(unix)]
fn create_destination(path: &Path, mkpath: bool) -> io::Result<()> {
    const ACCESSPERMS: u32 = 0o777;
    if fast_io::confinement::session_optout_allowed() {
        return create_destination_plainly(path, mkpath);
    }
    if is_confined_make_path(path, mkpath) {
        fast_io::make_path_beneath_cwd(path, ACCESSPERMS)
    } else if mkpath {
        fast_io::operator_make_path(path, ACCESSPERMS)
    } else {
        fast_io::operator_mkdir(path, ACCESSPERMS)
    }
}

#[cfg(not(unix))]
fn create_destination(path: &Path, mkpath: bool) -> io::Result<()> {
    create_destination_plainly(path, mkpath)
}

fn create_destination_plainly(path: &Path, mkpath: bool) -> io::Result<()> {
    if mkpath {
        fs::create_dir_all(path)
    } else {
        fs::create_dir(path)
    }
}

/// Names a failed destination creation the way upstream reports it.
///
/// The ownership walk refuses a foreign-owned symlink with `ELOOP`, after
/// printing upstream's refusal line; upstream reports that same refusal from
/// the `change_dir()` after its mkdir, so it keeps that line. Every other
/// failure is the mkdir's own: `mkdir %s failed`, RERR_FILEIO (main.c:819).
fn creation_error(path: &Path, mkpath: bool, error: io::Error) -> LocalCopyError {
    if is_symlink_refusal(path, mkpath, &error) {
        LocalCopyError::destination_entry_failed(created_destination_entry(mkpath), path, error)
    } else if mkpath {
        // upstream: main.c:760 - make_path()'s failure jumps to `mkdir_error`
        // before the trailing slash is cut, so the operand is named as given.
        LocalCopyError::destination_mkdir_failed(path, error)
    } else {
        // upstream: main.c:815 - the plain do_mkdir() runs after `*cp = '\0'`.
        LocalCopyError::destination_mkdir_failed(path.components().collect::<PathBuf>(), error)
    }
}

/// Whether `--mkpath` builds `path` through upstream's confined relative arm,
/// whose `ELOOP` is the mkdir's own failure rather than an ownership refusal.
#[cfg(unix)]
fn is_confined_make_path(path: &Path, mkpath: bool) -> bool {
    mkpath && path.is_relative()
}

#[cfg(unix)]
fn is_symlink_refusal(path: &Path, mkpath: bool, error: &io::Error) -> bool {
    !fast_io::confinement::session_optout_allowed()
        && !is_confined_make_path(path, mkpath)
        && error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_refusal(_path: &Path, _mkpath: bool, _error: &io::Error) -> bool {
    false
}

/// Computes the target path for a non-directory entry.
///
/// When an `--iconv` converter is supplied, the source filename component
/// `name` is transcoded with [`transcode_filename_component`] before being
/// appended to `destination_base`. This mirrors upstream rsync's
/// `flist.c:2000-2023` (sender) + `flist.c:1058-1072` (receiver) composition
/// in local-copy mode (`rsync.c:118-140`).
pub(super) fn compute_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
    is_directory: bool,
    iconv: Option<&FilenameConverter>,
) -> PathBuf {
    if destination_behaves_like_directory && (!prefer_root_destination || is_directory) {
        let converted = transcode_filename_component(name, iconv);
        destination_base.join(Path::new(&*converted))
    } else {
        destination_path.to_path_buf()
    }
}

/// Computes the target path for special entries (symlinks, FIFOs, devices).
///
/// These entries don't use the directory-specific logic that regular files use.
/// The `iconv` parameter applies the same LOCAL -> REMOTE transcoding the
/// per-directory path uses; see [`compute_target_path`].
pub(super) fn compute_special_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
    iconv: Option<&FilenameConverter>,
) -> PathBuf {
    if destination_behaves_like_directory && !prefer_root_destination {
        let converted = transcode_filename_component(name, iconv);
        destination_base.join(Path::new(&*converted))
    } else {
        destination_path.to_path_buf()
    }
}
