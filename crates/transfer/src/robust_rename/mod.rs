//! Temp-to-final commit rename with upstream's retry and cross-filesystem
//! fallback.
//!
//! A plain rename cannot cross a mount, so a `--temp-dir` on another
//! filesystem than the destination fails every commit with `EXDEV` unless the
//! file is copied across instead. Upstream splits that work between
//! `robust_rename()`, which chooses rename or copy, and `finish_transfer()`,
//! which moves a copy staged in a relative `--partial-dir` into place. The two
//! functions here keep that split.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/util1.c:596` `robust_rename()`.
//! - `rsync-3.5.0/rsync.c:882` `finish_transfer()`.

use std::io;
use std::path::{Path, PathBuf};

/// Attempts `robust_rename()` makes before giving up on a busy target.
///
/// upstream: `util1.c:599` `int tries = 4;`
const RENAME_TRIES: u32 = 4;

/// How [`robust_rename`] put the file in place.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Renamed {
    /// The rename succeeded. upstream: return value `0`.
    Moved,
    /// The rename crossed a filesystem and the file was copied to the held
    /// path, which is the relative partial-dir name when one was given.
    /// upstream: return value `1`.
    Copied(PathBuf),
}

/// The confinement the copy fallback resolves its endpoints under.
///
/// The same pair the commit rename resolves under, so the copy is confined
/// wherever that rename would have been.
#[derive(Clone, Copy)]
pub(crate) struct CommitAnchor<'a> {
    /// The receiver's destination sandbox, when one is plumbed.
    pub(crate) sandbox: Option<&'a fast_io::DirSandbox>,
    /// The destination root `sandbox` is anchored on.
    pub(crate) root: &'a Path,
}

/// Renames `from` to `to` with `rename`, falling back to copy-then-unlink
/// when the two sit on different filesystems.
///
/// `rename` is the caller's own confined rename, retried up to four times
/// while the target is a busy executable. On `EXDEV` the file is copied to
/// `partial` when given - after the partial directory is created - and to
/// `to` otherwise, then `from` is unlinked. The copy resolves both endpoints
/// under `anchor`.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/util1.c:606-632` - the retry loop and its `ETXTBSY` arm,
///   which unlinks the busy target and tries again.
/// - `rsync-3.5.0/util1.c:633-660` - the `EXDEV` arm: `handle_partial_dir(
///   partialptr, PDIR_CREATE)` and `to = partialptr`, then `copy_file()` and
///   `do_unlink_at(from)`.
///
/// # Errors
///
/// The rename error when it is neither `ETXTBSY` nor `EXDEV`, `ETXTBSY` once
/// the tries run out or the busy target cannot be removed, and any error
/// creating the partial directory or copying the file.
pub(crate) fn robust_rename(
    anchor: CommitAnchor<'_>,
    from: &Path,
    to: &Path,
    partial: Option<&Path>,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<Renamed> {
    let mut tries = RENAME_TRIES;
    loop {
        let error = match rename(from, to) {
            Ok(()) => return Ok(Renamed::Moved),
            Err(error) => error,
        };
        if error.kind() == io::ErrorKind::ExecutableFileBusy {
            unlink_busy_target(anchor, to)?;
            tries -= 1;
            if tries == 0 {
                return Err(error);
            }
            continue;
        }
        if !fast_io::is_cross_device(&error) {
            return Err(error);
        }
        let target = match partial {
            Some(partial) => {
                create_partial_dir(partial)?;
                partial
            }
            None => to,
        };
        fast_io::copy_then_unlink_via_sandbox_or_fallback(
            anchor.sandbox,
            anchor.root,
            from,
            target,
        )?;
        return Ok(Renamed::Copied(target.to_path_buf()));
    }
}

/// Puts a finished temp file in place, returning `true` when it was copied
/// rather than renamed so the caller re-applies its metadata.
///
/// A relative `partial_dir` is where a cross-filesystem copy is staged, so the
/// destination only ever changes by rename. The staged copy is then renamed
/// onto `to` and its emptied partial directory removed. An absolute
/// `partial_dir` is not used for staging.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/rsync.c:889` - `temp_copy_name` is `partialptr` only for a
///   relative `--partial-dir`.
/// - `rsync-3.5.0/rsync.c:918` - `robust_rename(fnametmp, fname,
///   temp_copy_name, ...)`.
/// - `rsync-3.5.0/rsync.c:948-955` - the staged copy is renamed onto `fname`
///   and `handle_partial_dir(temp_copy_name, PDIR_DELETE)` removes its dir.
///
/// # Errors
///
/// See [`robust_rename`], plus the error renaming the staged copy into place.
pub(crate) fn finish_rename(
    anchor: CommitAnchor<'_>,
    from: &Path,
    to: &Path,
    partial_dir: Option<&Path>,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<bool> {
    let temp_copy_name = partial_dir
        .filter(|dir| dir.is_relative())
        .and_then(|dir| crate::temp_guard::partial_dir_fname(to, dir));
    match robust_rename(anchor, from, to, temp_copy_name.as_deref(), &mut rename)? {
        Renamed::Moved => Ok(false),
        Renamed::Copied(copied) if copied == to => Ok(true),
        Renamed::Copied(staged) => {
            rename(&staged, to)?;
            engine::remove_partial_dir(partial_dir, &staged);
            Ok(true)
        }
    }
}

/// Creates the partial directory `partial` lives in.
///
/// upstream: `util1.c:1519-1534` `handle_partial_dir(..., PDIR_CREATE)` -
/// clear a non-directory at the name, then create it.
fn create_partial_dir(partial: &Path) -> io::Result<()> {
    let Some(dir) = partial.parent() else {
        return Ok(());
    };
    engine::clear_partial_dir_obstruction(dir)?;
    engine::create_partial_dir(dir)
}

/// Removes a busy rename target so the next try can succeed.
///
/// upstream: `util1.c:625-631` - `robust_unlink(to)`, reporting `ETXTBSY`
/// when that fails.
fn unlink_busy_target(anchor: CommitAnchor<'_>, to: &Path) -> io::Result<()> {
    let relative = to.strip_prefix(anchor.root).unwrap_or(to);
    fast_io::unlink_via_sandbox_or_fallback(
        anchor.sandbox,
        anchor.root,
        relative,
        to,
        fast_io::UnlinkFlags::File,
    )
    .map_err(|_| io::Error::from(io::ErrorKind::ExecutableFileBusy))
}

#[cfg(test)]
mod tests;
