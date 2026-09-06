//! The local-copy path's single decision site for a DIRECTORY standing where a
//! regular file, symlink, FIFO, socket, or device node has to be written.
//!
//! Upstream reaches this decision from two call sites, and neither of them
//! gates the removal on an option:
//!
//! ```text
//! generator.c:2148-2153            (a regular file is arriving)
//! if (statret == 0 && !(stype == FT_REG || (write_devices && stype == FT_DEVICE))) {
//!         if (delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_FILE) != 0)
//!                 goto cleanup;
//!         statret = -1;
//!
//! generator.c:2477-2483            (atomic_create(), a symlink/device/special)
//! if (make_backups > 0 && !dir_in_the_way) {
//!         if (!make_backup(fname, skip_atomic))
//!                 return 0;
//! } else if (skip_atomic) {
//!         int del_opts = delete_mode || force_delete ? DEL_RECURSE : 0;
//!         if (delete_item(fname, sxp->st.st_mode, del_opts | del_for_flag) != 0)
//!                 return 0;
//! }
//! ```
//!
//! `del_opts` carries `DEL_RECURSE` only under `--delete` / `--force`, and
//! `DEL_RECURSE` selects the RECURSION, not the removal: `delete_item()` runs
//! either way and `delete_dir_contents()` "just reports emptiness" when the flag
//! is clear (`delete.c:207-209`). So an EMPTY directory obstacle is `rmdir`'d
//! and the replacement placed at exit 0 with no options at all, and a POPULATED
//! one is refused OUT LOUD - `cannot delete non-empty directory` on stdout,
//! `could not make way for new <noun>` on stderr, that one entry skipped, and
//! the run finishing `RERR_PARTIAL` (23) with every other entry transferred.
//!
//! oc's local-copy executor gated the whole removal on `--force` / `--delete`
//! instead, so without them a regular file arriving over a destination
//! directory was dropped in silence at exit 0, and a symlink or special aborted
//! the run with an oc-only argument error that has no upstream analogue.
//!
//! `dir_in_the_way` also forces `skip_atomic` (`generator.c:2469-2472`), which
//! is why the `make_backup` arm above is unreachable for a directory in EITHER
//! mode. `delete_item()` then splits the same way one level down - the `rmdir`
//! arm at `delete.c:221-226`, the `make_backup`/`unlink` arm in the `else` at
//! `delete.c:227-238` - so the directory NODE is never backed up. Its CONTENTS
//! are: the `DEL_RECURSE` peel calls `delete_item()` per child, and that child
//! takes the `make_backup` arm under `--backup`. With a `--backup-dir` the
//! children move out and the `rmdir` then succeeds; with a plain `~` suffix they
//! are renamed in place, the directory never empties, and upstream refuses at 23
//! even though `--force` was given. Both are measured cells, not readings.
//!
//! # Upstream Reference
//!
//! - `generator.c:2148-2153` - the `DEL_FOR_FILE` removal ahead of a regular-file transfer
//! - `generator.c:2469-2483` - `atomic_create()`'s `dir_in_the_way` / `skip_atomic` / `del_opts`
//! - `delete.c:204-214` - `delete_dir_contents()` reached first for a directory
//! - `delete.c:221-226` - the `rmdir` arm
//! - `delete.c:259-268` - the `ENOTEMPTY` / `rsyserr` / `ENOENT` split
//! - `delete.c:272-286` - `could not make way for %s %s: %s` at `FERROR_XFER`
//! - `log.c:310-311` - `case FERROR_XFER: got_xfer_error = 1;`
//! - `cleanup.c:217-218` - `got_xfer_error` lifts a zero exit to `RERR_PARTIAL`

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::cleanup::is_dir_not_empty;
use crate::local_copy::{CopyContext, LocalCopyError};

/// The kind of item the local-copy executor is clearing the way for.
///
/// Names the `DEL_FOR_*` flag upstream ORs into `delete_item`'s flags, which
/// selects the noun in the `could not make way for new %s: %s` diagnostic.
/// Upstream picks it from the *new* entry's type, not the obstacle's.
///
/// # Upstream Reference
///
/// - `delete.c:275-282` - the `DEL_MAKE_ROOM` switch that picks the noun
#[derive(Clone, Copy)]
pub(crate) enum MakeWayFor {
    /// upstream `DEL_FOR_FILE` - `"regular file"`.
    File,
    /// upstream `DEL_FOR_SYMLINK` - `"symlink"`.
    Symlink,
    /// upstream `DEL_FOR_DEVICE` - `"device file"`.
    Device,
    /// upstream `DEL_FOR_SPECIAL` - `"special file"`.
    Special,
}

impl MakeWayFor {
    /// upstream: `delete.c:275-279` - the `desc` assigned per `DEL_FOR_*`.
    const fn description(self) -> &'static str {
        match self {
            Self::File => "regular file",
            Self::Symlink => "symlink",
            Self::Device => "device file",
            Self::Special => "special file",
        }
    }
}

/// Removes a directory standing where `make_way_for` has to be written.
///
/// Returns `Ok(true)` when the path is clear and the caller may create the
/// replacement, `Ok(false)` when the obstacle survived - the caller then skips
/// that entry, exactly as upstream's `goto cleanup` / `return 0` does, and the
/// run finishes `RERR_PARTIAL` (23) with every other entry transferred.
///
/// `recurse` is upstream's `DEL_RECURSE`: `delete_mode || force_delete`. It
/// selects whether the contents are peeled first, never whether the `rmdir` is
/// attempted.
pub(crate) fn clear_directory_obstacle(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    recurse: bool,
    make_way_for: MakeWayFor,
) -> Result<bool, LocalCopyError> {
    // upstream: delete.c:204-214 - `delete_item()` calls
    // `delete_dir_contents()` FIRST for a directory, and a DR_NOT_EMPTY from
    // there short-circuits the `rmdir` entirely. The emptiness probe is a
    // `get_dirlist()` readdir (delete.c:110-117), not an errno, so it still
    // refuses under `--dry-run` - where the `rmdir` itself would have returned
    // 0 without touching anything (`syscall.c do_rmdir_at()`'s `if (dry_run)
    // return 0`).
    if !peel_or_probe(context, destination, relative, recurse)? {
        report_not_empty(destination, relative);
        report_make_way_failure(context, destination, relative, make_way_for);
        return Ok(false);
    }

    if context.mode().is_dry_run() {
        context.register_progress();
        return Ok(true);
    }

    match fs::remove_dir(destination) {
        Ok(()) => {
            context.register_progress();
            Ok(true)
        }
        // upstream: delete.c:267 - `errno == ENOENT` maps to DR_SUCCESS.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            context.register_progress();
            Ok(true)
        }
        Err(error) => {
            if is_dir_not_empty(&error) {
                // upstream: delete.c:260-262 - the SECOND site the same notice
                // comes from. A `--backup` peel renames each child in place, so
                // `delete_dir_contents()` returns DR_SUCCESS and only the rmdir
                // errno reveals that the directory refilled itself.
                report_not_empty(destination, relative);
            } else {
                // upstream: delete.c:264-266 - rsyserr(FERROR_XFER, errno,
                // "delete_file: %s(%s) failed", what, fbuf).
                eprintln!(
                    "rsync: [receiver] delete_file: rmdir({}) failed: {}",
                    display_name(destination, relative),
                    crate::local_copy::upstream_io_error(&error),
                );
            }
            report_make_way_failure(context, destination, relative, make_way_for);
            Ok(false)
        }
    }
}

/// Runs `delete_dir_contents()`: the emptiness probe when `recurse` is clear,
/// the itemize-and-peel when it is set.
///
/// Returns `false` for upstream's `DR_NOT_EMPTY`, which the caller turns into
/// the notice plus the make-way refusal without ever reaching the `rmdir`.
///
/// # Upstream Reference
///
/// - `delete.c:110-118` - `get_dirlist()`, then `if (!dirlist->used) goto done`
/// - `delete.c:115-118` - `if (!(flags & DEL_RECURSE)) { ret = DR_NOT_EMPTY; goto done; }`
fn peel_or_probe(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    recurse: bool,
) -> Result<bool, LocalCopyError> {
    if !recurse {
        return directory_is_empty(destination);
    }

    // upstream: delete.c:141-163 - the children are reported like delete-pass
    // deletions (DEL_MAKE_ROOM is stripped for them) while the directory node
    // itself stays silent.
    context.record_make_room_contents(destination, relative)?;
    if !context.mode().is_dry_run() {
        clear_contents(context, destination, relative)?;
    }
    Ok(true)
}

/// Reports whether `directory` holds no entries at all.
///
/// A directory that vanished counts as empty: upstream maps the `ENOENT` that
/// follows to `DR_SUCCESS` (`delete.c:267`).
fn directory_is_empty(directory: &Path) -> Result<bool, LocalCopyError> {
    match fs::read_dir(directory) {
        Ok(mut entries) => match entries.next() {
            None => Ok(true),
            Some(Ok(_)) => Ok(false),
            Some(Err(error)) => Err(LocalCopyError::io("read directory", directory, error)),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(LocalCopyError::io("read directory", directory, error)),
    }
}

/// Emits `cannot delete non-empty directory: %s`.
///
/// `FINFO` carries no `INFO_GTE` gate at either upstream site, so it prints at
/// default verbosity and is suppressed only by `--quiet`; level 0 keeps
/// `info_gte` trivially true and leaves the quiet check to the renderer.
///
/// # Upstream Reference
///
/// - `delete.c:178-181` - the `delete_dir_contents()` site
/// - `delete.c:260-262` - the `rmdir`-errno site
fn report_not_empty(destination: &Path, relative: Option<&Path>) {
    logging::info_log!(
        Del,
        0,
        "cannot delete non-empty directory: {}",
        display_name(destination, relative)
    );
}

/// Emits `could not make way for new %s: %s` and records the transfer error.
///
/// The line is `FERROR_XFER`, which sets `got_xfer_error` and so lifts a zero
/// exit to `RERR_PARTIAL` (23) without aborting the run.
///
/// # Upstream Reference
///
/// - `delete.c:272-286` - the `DEL_MAKE_ROOM` block reached via `check_ret`
fn report_make_way_failure(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    make_way_for: MakeWayFor,
) {
    eprintln!(
        "could not make way for new {}: {}",
        make_way_for.description(),
        display_name(destination, relative),
    );
    context.record_make_way_error();
}

/// Renders the transfer-relative name upstream's `f_name()` would print, not
/// the absolute destination path.
fn display_name(destination: &Path, relative: Option<&Path>) -> String {
    relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from))
        .unwrap_or_else(|| destination.to_path_buf())
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Peels a directory obstacle's contents the way `delete_dir_contents()` does.
///
/// Each child reaches `delete_item()` in turn, so a non-directory child takes
/// the `make_backup` arm under `--backup` rather than being unlinked. That is
/// what makes a `--backup-dir` run empty the directory (the children move out)
/// and a plain-suffix run fail to (`child` becomes `child~` in place) - the
/// caller's `rmdir` then reports whichever happened.
///
/// A sub-directory that survives its own `rmdir` for the same reason is left
/// standing rather than raised: upstream's `DR_NOT_EMPTY` is not a failure, and
/// the caller's `rmdir` of the parent is what turns it into the operator-facing
/// refusal.
///
/// # Upstream Reference
///
/// - `delete.c:91-181` - `delete_dir_contents()`
/// - `delete.c:227-238` - the `make_backup` / `unlink` arm `delete_item()` takes per child
fn clear_contents(
    context: &mut CopyContext,
    directory: &Path,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    // Read the whole directory before mutating it: an in-place `--backup`
    // renames `child` to `child~` inside this very directory, and a live
    // `ReadDir` may or may not hand the new name back depending on the
    // filesystem.
    let mut children = Vec::new();
    match fs::read_dir(directory) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .map_err(|error| LocalCopyError::io("read directory", directory, error))?;
                let file_type = entry.file_type().map_err(|error| {
                    LocalCopyError::io("inspect directory entry", directory, error)
                })?;
                children.push((entry.file_name(), entry.path(), file_type));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(LocalCopyError::io("read directory", directory, error)),
    }

    for (name, path, file_type) in children {
        let child_relative = relative.map(|parent| parent.join(&name));
        if file_type.is_dir() {
            clear_contents(context, &path, child_relative.as_deref())?;
            // DR_NOT_EMPTY here is benign; the parent's rmdir reports it.
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound || is_dir_not_empty(&error) => {}
                Err(error) => {
                    return Err(LocalCopyError::io("remove existing directory", path, error));
                }
            }
            continue;
        }

        // upstream: delete.c:228 - `make_backups > 0 && !(flags &
        // DEL_FOR_BACKUP) && (backup_dir || !is_backup_file(fbuf))`.
        if context.options().should_backup_before_delete(&name) {
            // upstream: delete.c:230 - `make_backup(fbuf, True)`; the item is
            // unlinked outright right after whichever strategy placed the
            // backup, so the hard-link tier is skipped.
            context.backup_existing_entry(&path, child_relative.as_deref(), file_type, true)?;
        }

        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "remove existing destination",
                    path,
                    error,
                ));
            }
        }
    }

    Ok(())
}
