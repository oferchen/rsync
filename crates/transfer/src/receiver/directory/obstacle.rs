//! The receiver's single decision site for an existing destination entry that
//! stands where a regular file, symlink, FIFO, socket, or device node has to
//! be written.
//!
//! Upstream makes this decision in exactly one place. `atomic_create()` sets
//! `dir_in_the_way` when the obstacle is a directory, which also forces
//! `skip_atomic`, so a directory obstacle reaches `delete_item()` whether or
//! not `--backup` is set - the `make_backup` arm is unreachable for it in
//! either mode. `delete_item()` then splits on the same test one level down:
//! the `rmdir` arm for a directory, the `make_backup`/`unlink` arm in the
//! `else`. Keeping both arms in one function here is what makes oc's structure
//! answer upstream's; deciding the directory case inside the backup path would
//! leave every `--backup`-off shape decided somewhere else.
//!
//! The regular-file arm reaches the same `delete_item()` from its own call
//! site rather than through `atomic_create()`: `generator.c:2149` removes a
//! destination that is not a regular file (nor a device under
//! `--write-devices`) before the delta is requested, then sets `statret = -1`
//! so everything below sees an absent destination. Writing through such an
//! obstacle - or renaming over it - is not the same operation: `--backup`
//! never sees it, and a directory is never cleared at all.
//!
//! # Upstream Reference
//!
//! - `generator.c:2148-2153` - the `DEL_FOR_FILE` removal ahead of a regular-file transfer
//! - `generator.c:2469` - `int skip_atomic, dir_in_the_way = del_for_flag && S_ISDIR(sxp->st.st_mode);`
//! - `generator.c:2471-2472` - `dir_in_the_way` forces `skip_atomic = 1`
//! - `generator.c:2477-2483` - `if (make_backups > 0 && !dir_in_the_way) make_backup(...)`
//!   `else if (skip_atomic) delete_item(fname, sxp->st.st_mode, del_opts | del_for_flag)`
//! - `delete.c:222-226` - the `rmdir` arm for `S_ISDIR(mode)`
//! - `delete.c:227-238` - the `make_backup`/`unlink` arm in the `else`
//! - `delete.c:178-181` - `cannot delete non-empty directory: %s` at `FINFO`
//! - `delete.c:273-286` - `could not make way for %s %s: %s` at `FERROR_XFER`
//!
//! # Known residual: `DEL_RECURSE`
//!
//! `generator.c:2481` computes `int del_opts = delete_mode || force_delete ?
//! DEL_RECURSE : 0`, so under `--delete` or `--force` upstream removes a
//! POPULATED directory obstacle recursively and completes at exit 0. Measured
//! against 3.5.0 over a real `--server` child: `--force`, `--delete`, and both
//! together each give `rc=0` with the symlink in place, where this arm refuses
//! at 23.
//!
//! Neither flag is reachable from here. `--force` is parsed and discarded by
//! the server arg parser (`cli/src/frontend/server/flags.rs:615`, `"--force"
//! => {}`) and has no field in `ParsedServerFlags`, so the receiver cannot
//! know it was asked for. Wiring it is a server-arg and config change, not an
//! obstacle-removal one, and is deliberately out of scope here: before this
//! module existed the same shape aborted the whole transfer at 12 with the
//! sibling files unsent, so the refusal below is strictly closer to upstream,
//! not a new divergence.

#[cfg(any(unix, windows))]
use std::io;
#[cfg(any(unix, windows))]
use std::path::Path;

#[cfg(any(unix, windows))]
use crate::pipeline::receiver::upstream_errno_text;
#[cfg(any(unix, windows))]
use crate::receiver::ReceiverContext;

/// The kind of item the receiver is clearing the way for.
///
/// Names the `DEL_FOR_*` flag upstream ORs into `delete_item`'s flags, which
/// selects the noun in the `could not make way for new %s: %s` diagnostic.
/// Upstream picks it from the *new* entry's type, not the obstacle's
/// (`generator.c:2041-2047`).
///
/// # Upstream Reference
///
/// - `delete.c:275-282` - the `DEL_MAKE_ROOM` switch that picks the noun
#[cfg(any(unix, windows))]
#[derive(Clone, Copy)]
pub(in crate::receiver) enum MakeWayFor {
    /// upstream `DEL_FOR_FILE` - `"regular file"`.
    File,
    /// upstream `DEL_FOR_SYMLINK` - `"symlink"`.
    Symlink,
    /// upstream `DEL_FOR_DEVICE` - `"device file"`.
    ///
    /// Unix only: `create_specials` is the sole constructor and Windows has
    /// no `mknod` path, so on Windows this variant is unconstructible and
    /// `-D warnings` rejects it as dead code.
    #[cfg(unix)]
    Device,
    /// upstream `DEL_FOR_SPECIAL` - `"special file"`.
    ///
    /// Unix only, for the same reason as [`Self::Device`].
    #[cfg(unix)]
    Special,
}

#[cfg(any(unix, windows))]
impl MakeWayFor {
    /// upstream: `delete.c:275-279` - the `desc` assigned per `DEL_FOR_*`.
    const fn description(self) -> &'static str {
        match self {
            Self::File => "regular file",
            Self::Symlink => "symlink",
            #[cfg(unix)]
            Self::Device => "device file",
            #[cfg(unix)]
            Self::Special => "special file",
        }
    }
}

/// Reports whether `error` is the kernel's "directory is not empty" refusal.
///
/// Linux and the BSDs disagree on the errno `rmdir(2)` raises for a populated
/// directory, and upstream tests only `ENOTEMPTY` because its own `rmdir` path
/// never sees the BSD spelling. Accepting both keeps the diagnostic identical
/// across the platforms oc builds for.
///
/// upstream: `delete.c:260-263` - `if (S_ISDIR(mode) && errno == ENOTEMPTY)`
#[cfg(unix)]
fn is_not_empty(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTEMPTY) | Some(libc::EEXIST)
    )
}

/// Windows reports a populated directory as `ERROR_DIR_NOT_EMPTY`, which `std`
/// surfaces as a raw OS error rather than a named `io::ErrorKind`.
#[cfg(windows)]
fn is_not_empty(error: &io::Error) -> bool {
    const ERROR_DIR_NOT_EMPTY: i32 = 145;
    error.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY)
}

#[cfg(any(unix, windows))]
impl ReceiverContext {
    /// Clears whatever stands at `existing` so a fresh entry can be written
    /// there, reporting any refusal the way upstream reports it.
    ///
    /// `Ok(())` means the path is free: it was already absent, it was removed,
    /// or it was moved into the backup area. `Err` means the entry must be
    /// skipped; the diagnostic has already been emitted, so the caller only has
    /// to `continue`.
    ///
    /// The directory arm never consults `--backup`: upstream's `dir_in_the_way`
    /// excludes a directory obstacle from `make_backup` in both modes and sends
    /// it to `delete_item`'s `rmdir`, which clears an empty directory and
    /// refuses a populated one.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2469-2485` - `atomic_create()`'s `dir_in_the_way` split
    /// - `delete.c:205-239` - `delete_item()`'s `rmdir` and `make_backup` arms
    #[cfg(unix)]
    pub(in crate::receiver) fn make_way_for_replacement<W>(
        &self,
        writer: &mut W,
        existing: &Path,
        relative_path: &Path,
        dest_dir: &Path,
        sandbox: Option<&fast_io::DirSandbox>,
        make_way_for: MakeWayFor,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        // SEC-1.f: when the sandbox is plumbed and the destination parent is
        // the sandbox root the obstacle stat goes through
        // `fstatat(AT_SYMLINK_NOFOLLOW)`, so a TOCTOU symlink swap on
        // `existing` cannot redirect the probe to a different inode.
        let Ok(metadata) =
            fast_io::lstat_via_sandbox_or_fallback(sandbox, dest_dir, relative_path, existing)
        else {
            // upstream: delete.c:268-269 maps ENOENT to DR_SUCCESS and
            // backup.c:236 returns "nothing to keep" - either way there is
            // nothing in the way to make way for.
            return Ok(());
        };

        if metadata.is_dir() {
            // upstream: generator.c:2469 - dir_in_the_way, so neither the
            // backup arm nor the atomic temp-and-rename applies.
            return self.rmdir_obstacle(
                writer,
                existing,
                relative_path,
                dest_dir,
                sandbox,
                make_way_for,
            );
        }

        // upstream: generator.c:2477 / delete.c:228-230 - the backup arm, which
        // the `is_dir` test above has already excluded a directory from.
        match self.backup_existing_before_replace(existing, relative_path, dest_dir, sandbox) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return self.report_backup_failure(writer, relative_path, error),
        }

        // SEC-1.g: the obstacle unlink is anchored on the same sandbox dirfd
        // the stat above used, so a swap between the two cannot redirect the
        // syscall to an attacker-chosen parent.
        //
        // upstream: delete.c:236-237 - `what = "unlink"; ok = del_unlink(fbuf) == 0;`
        match fast_io::unlink_via_sandbox_or_fallback(
            sandbox,
            dest_dir,
            relative_path,
            existing,
            fast_io::UnlinkFlags::File,
        ) {
            Ok(()) => Ok(()),
            Err(error) => self.report_unlink_failure(writer, relative_path, make_way_for, error),
        }
    }

    /// Windows variant: no dirfd sandbox, so the probe and the removal are
    /// path-based, matching the Windows symlink-create path.
    #[cfg(windows)]
    pub(in crate::receiver) fn make_way_for_replacement<W>(
        &self,
        writer: &mut W,
        existing: &Path,
        relative_path: &Path,
        dest_dir: &Path,
        make_way_for: MakeWayFor,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let Ok(metadata) = std::fs::symlink_metadata(existing) else {
            return Ok(());
        };

        if metadata.is_dir() {
            return self.rmdir_obstacle(writer, existing, relative_path, make_way_for);
        }

        match self.backup_existing_before_replace(existing, relative_path, dest_dir) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return self.report_backup_failure(writer, relative_path, error),
        }

        match std::fs::remove_file(existing) {
            Ok(()) => Ok(()),
            Err(error) => self.report_unlink_failure(writer, relative_path, make_way_for, error),
        }
    }

    /// The directory arm: `rmdir`, plus upstream's two-line refusal when the
    /// directory is not empty.
    ///
    /// Upstream reaches `rmdir` for this shape because `atomic_create` passes
    /// `del_opts` without `DEL_RECURSE` unless `--delete`/`--force` is in play,
    /// so `delete_dir_contents` only probes emptiness and reports it; the
    /// contents are never removed to make room.
    ///
    /// # Upstream Reference
    ///
    /// - `delete.c:222-226` - `what = "rmdir"; ok = do_rmdir_at(fbuf) == 0;`
    #[cfg(unix)]
    fn rmdir_obstacle<W>(
        &self,
        writer: &mut W,
        existing: &Path,
        relative_path: &Path,
        dest_dir: &Path,
        sandbox: Option<&fast_io::DirSandbox>,
        make_way_for: MakeWayFor,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        match fast_io::unlink_via_sandbox_or_fallback(
            sandbox,
            dest_dir,
            relative_path,
            existing,
            fast_io::UnlinkFlags::Dir,
        ) {
            Ok(()) => Ok(()),
            Err(error) => self.report_rmdir_failure(writer, relative_path, make_way_for, error),
        }
    }

    /// Windows variant of [`Self::rmdir_obstacle`].
    #[cfg(windows)]
    fn rmdir_obstacle<W>(
        &self,
        writer: &mut W,
        existing: &Path,
        relative_path: &Path,
        make_way_for: MakeWayFor,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        match std::fs::remove_dir(existing) {
            Ok(()) => Ok(()),
            Err(error) => self.report_rmdir_failure(writer, relative_path, make_way_for, error),
        }
    }

    /// Renders a failed `rmdir` the way `delete_item` does.
    ///
    /// A vanished directory is not a failure - upstream maps `ENOENT` to
    /// `DR_SUCCESS` and creates the replacement. A populated one gets the
    /// `FINFO` emptiness notice before the `FERROR_XFER` refusal; any other
    /// errno gets `delete_file: rmdir(...) failed` in its place.
    ///
    /// # Upstream Reference
    ///
    /// - `delete.c:259-268` - the `ENOTEMPTY` / `rsyserr` / `ENOENT` split
    fn report_rmdir_failure<W>(
        &self,
        writer: &mut W,
        relative_path: &Path,
        make_way_for: MakeWayFor,
        error: io::Error,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        if is_not_empty(&error) {
            // upstream: delete.c:178-181 - delete_dir_contents() reports the
            // emptiness probe at FINFO before delete_item() names the item it
            // blocked. FINFO leaves the exit code alone; the FERROR_XFER line
            // that follows is what lifts it to RERR_PARTIAL.
            let _ = self.emit_info_line(
                writer,
                &format!(
                    "cannot delete non-empty directory: {}\n",
                    relative_path.display()
                ),
            );
        } else {
            // upstream: delete.c:264-266 - rsyserr(FERROR_XFER, errno,
            // "delete_file: %s(%s) failed", what, fbuf).
            let _ = self.emit_error_xfer_line(
                writer,
                &format!(
                    "rsync: [receiver] delete_file: rmdir({}) failed: {}\n",
                    relative_path.display(),
                    upstream_errno_text(&error)
                ),
            );
        }
        self.report_make_way_failure(writer, relative_path, make_way_for, error)
    }

    /// Renders a failed obstacle `unlink` the way `delete_item` does.
    ///
    /// # Upstream Reference
    ///
    /// - `delete.c:264-266` - `rsyserr(FERROR_XFER, errno, "delete_file: %s(%s) failed", ...)`
    fn report_unlink_failure<W>(
        &self,
        writer: &mut W,
        relative_path: &Path,
        make_way_for: MakeWayFor,
        error: io::Error,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        let _ = self.emit_error_xfer_line(
            writer,
            &format!(
                "rsync: [receiver] delete_file: unlink({}) failed: {}\n",
                relative_path.display(),
                upstream_errno_text(&error)
            ),
        );
        self.report_make_way_failure(writer, relative_path, make_way_for, error)
    }

    /// Emits `could not make way for new %s: %s` and returns the error so the
    /// caller skips the entry.
    ///
    /// The line is `FERROR_XFER`, which sets the peer's `got_xfer_error` and so
    /// lifts a zero exit to `RERR_PARTIAL` (23). Reporting the same event at a
    /// verbosity-gated `debug_log!` instead - which is what these call sites
    /// used to do - leaves the run exiting 0 with the obstacle still standing.
    ///
    /// # Upstream Reference
    ///
    /// - `delete.c:272-286` - the `DEL_MAKE_ROOM` block reached via `check_ret`
    /// - `log.c:310-311` - `case FERROR_XFER: got_xfer_error = 1;`
    /// - `cleanup.c:217-218` - `got_xfer_error` lifts a zero exit to `RERR_PARTIAL`
    fn report_make_way_failure<W>(
        &self,
        writer: &mut W,
        relative_path: &Path,
        make_way_for: MakeWayFor,
        error: io::Error,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let _ = self.emit_error_xfer_line(
            writer,
            &format!(
                "could not make way for new {}: {}\n",
                make_way_for.description(),
                relative_path.display()
            ),
        );
        Err(error)
    }

    /// Reports a backup mechanism that could not preserve the obstacle.
    ///
    /// Upstream is loud here and skips the entry: every `return 0` path in
    /// `make_backup_inner()` has already called `rsyserr` (or `copy_valid_path`
    /// has), and `atomic_create` then returns 0 without creating the
    /// replacement. `FERROR` rather than `FERROR_XFER` matches upstream, which
    /// leaves `got_xfer_error` clear on this arm.
    ///
    /// # Upstream Reference
    ///
    /// - `backup.c:400-401` - `rsyserr(FERROR, errno, "keep_backup failed: %s -> \"%s\"", ...)`
    /// - `generator.c:2478-2479` - `if (!make_backup(fname, skip_atomic)) return 0;`
    fn report_backup_failure<W>(
        &self,
        writer: &mut W,
        relative_path: &Path,
        error: io::Error,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let _ = self.emit_error_line(
            writer,
            &format!(
                "rsync: [receiver] keep_backup failed: {}: {}\n",
                relative_path.display(),
                upstream_errno_text(&error)
            ),
        );
        Err(error)
    }
}
