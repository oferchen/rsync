//! Receiver-side handler for `--delete-missing-args` mode-0 sentinel entries.
//!
//! When the sender emits a mode-0 sentinel entry for a vanished top-level
//! source (`flist.c:2491-2497`), the receiver must delete the corresponding
//! destination path if it exists and skip any further processing for that
//! entry. Without this handler the sentinel survives in the file list but
//! triggers no filesystem action, leaving stale destination state behind
//! and producing the observable "missing-arg file was not deleted"
//! divergence against upstream rsync.
//!
//! # Upstream Reference
//!
//! - `generator.c:1360-1366` - `if (missing_args == 2 && file->mode == 0)`:
//!   apply the filter list, then `delete_item()` when `statret == 0`.
//! - `flist.c:2491-2497` - `missing_args == 2` sender branch that emits
//!   the mode-0 sentinel this handler consumes.

use std::io;
use std::path::Path;

use logging::{debug_log, info_log};

use super::obstacle::is_not_empty;
use crate::receiver::ReceiverContext;

impl ReceiverContext {
    /// Processes mode-0 sentinel entries injected by the sender's
    /// `--delete-missing-args` (`missing_args == 2`) branch.
    ///
    /// For every entry whose `mode == 0` we look up the corresponding
    /// destination path relative to `dest_dir` and remove it if it exists.
    /// Mode-0 entries carry no usable file type bits (the sender writes a
    /// raw zero), so we dispatch on the destination filesystem's symlink
    /// metadata: a directory is emptied first only under `DEL_RECURSE`
    /// (`--delete` or `--force`), otherwise a populated one is kept with
    /// `cannot delete non-empty directory: %s`; everything else is unlinked.
    /// Missing destinations are a no-op, matching upstream's `statret == 0`
    /// guard.
    ///
    /// All filesystem mutations route through the sandbox helpers
    /// ([`fast_io::unlink_via_sandbox_or_fallback`],
    /// [`fast_io::recursive_unlinkat_via_sandbox_or_fallback`]) so a TOCTOU
    /// swap on a single-component leaf under the destination root cannot
    /// redirect the deletion to an attacker-chosen path. Multi-component
    /// relative paths take the documented path-based fallback inside the
    /// helper (see `crates/fast_io/src/dir_sandbox/at_syscalls/`).
    ///
    /// # No-op conditions
    ///
    /// - `delete_missing_args` is not in effect on the receiver config.
    /// - `dry_run` is in effect (no filesystem mutations).
    /// - The destination path does not exist (`ENOENT` is silently
    ///   swallowed, mirroring upstream's `statret == 0` guard).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1360-1366` - `missing_args == 2 && file->mode == 0`
    ///   branch that calls `delete_item()` for an existing destination
    ///   and falls through (no creation) for a missing destination.
    /// - `generator.c:1629` - `del_opts` carries `DEL_RECURSE` only for
    ///   `delete_mode || force_delete`.
    /// - `delete.c:115-118`, `delete.c:178-181` - without `DEL_RECURSE` a
    ///   populated directory is `DR_NOT_EMPTY`, reported at `FINFO` and kept.
    pub(in crate::receiver) fn process_missing_args_sentinels<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> io::Result<()> {
        self.process_missing_args_sentinels_in_range(
            0..self.file_list.len(),
            dest_dir,
            #[cfg(unix)]
            sandbox,
            writer,
        )
    }

    /// [`process_missing_args_sentinels`](Self::process_missing_args_sentinels)
    /// restricted to the flat-index range `[range.start, range.end)`. Upstream
    /// handles each sentinel inline in `recv_generator()` (generator.c:1749-1755).
    pub(in crate::receiver) fn process_missing_args_sentinels_in_range<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        range: std::ops::Range<usize>,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> io::Result<()> {
        if !self.config.file_selection.delete_missing_args {
            return Ok(());
        }
        if self.config.flags.skip_dest_writes() {
            return Ok(());
        }

        for entry in &self.file_list[range] {
            // upstream: generator.c:1348 - sentinel is identified by mode == 0.
            if entry.mode() != 0 {
                continue;
            }

            let relative = entry.path();
            // Defensive: never act on the implicit root entry.
            if relative.as_os_str().is_empty() || relative.as_os_str() == "." {
                continue;
            }

            let target = dest_dir.join(relative);

            // upstream: generator.c:1351 - `statret == 0`: only delete an
            // existing destination. `symlink_metadata` here mirrors
            // `link_stat()` so a symlink is removed rather than followed.
            let metadata = match std::fs::symlink_metadata(&target) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => {
                    debug_log!(
                        Del,
                        1,
                        "delete-missing-args: stat {} failed: {}",
                        target.display(),
                        err,
                    );
                    continue;
                }
            };

            #[cfg(unix)]
            let sandbox_ref = sandbox;

            let is_dir = metadata.is_dir();
            let result = if is_dir && !self.del_recurse() {
                // upstream: generator.c:1749-1753 hands the sentinel to
                // `delete_item(fname, mode, del_opts)`, and without
                // `DEL_RECURSE` (generator.c:1629) `delete_dir_contents()`
                // refuses any populated directory (delete.c:115-118). A plain
                // rmdir gives the same answer: it fails ENOTEMPTY exactly then.
                #[cfg(unix)]
                {
                    fast_io::unlink_via_sandbox_or_fallback(
                        sandbox_ref,
                        dest_dir,
                        relative,
                        &target,
                        fast_io::UnlinkFlags::Dir,
                    )
                }
                #[cfg(not(unix))]
                {
                    std::fs::remove_dir(&target)
                }
            } else if is_dir {
                #[cfg(unix)]
                {
                    fast_io::recursive_unlinkat_via_sandbox_or_fallback(
                        sandbox_ref,
                        dest_dir,
                        relative,
                        &target,
                    )
                }
                #[cfg(not(unix))]
                {
                    std::fs::remove_dir_all(&target)
                }
            } else {
                #[cfg(unix)]
                {
                    fast_io::unlink_via_sandbox_or_fallback(
                        sandbox_ref,
                        dest_dir,
                        relative,
                        &target,
                        fast_io::UnlinkFlags::File,
                    )
                }
                #[cfg(not(unix))]
                {
                    std::fs::remove_file(&target)
                }
            };

            match result {
                Ok(()) => {
                    if is_dir {
                        // upstream: log.c:845 log_delete uses one "deleting %n"
                        // form; %n (log.c:633-641) appends a trailing slash for
                        // directories, so a dir prints "deleting sub/" - no word
                        // "directory".
                        info_log!(Del, 1, "deleting {}/", target.display());
                    } else {
                        info_log!(Del, 1, "deleting {}", target.display());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) if is_dir && is_not_empty(&err) => {
                    // upstream: delete.c:178-181 - FINFO, the directory is
                    // kept and the exit code is left alone.
                    let _ = self.emit_info_line(
                        writer,
                        &format!(
                            "cannot delete non-empty directory: {}\n",
                            relative.display()
                        ),
                    );
                }
                Err(err) => {
                    debug_log!(
                        Del,
                        1,
                        "delete-missing-args: failed to delete {}: {}",
                        target.display(),
                        err,
                    );
                }
            }
        }

        Ok(())
    }
}
