//! Receiver-side handler for `--delete-missing-args` mode-0 sentinel entries.
//!
//! When the sender emits a mode-0 sentinel entry for a vanished top-level
//! source (`flist.c:2254-2258`), the receiver must delete the corresponding
//! destination path if it exists and skip any further processing for that
//! entry. Without this handler the sentinel survives in the file list but
//! triggers no filesystem action, leaving stale destination state behind
//! and producing the observable "missing-arg file was not deleted"
//! divergence against upstream rsync.
//!
//! # Upstream Reference
//!
//! - `generator.c:1348-1354` - `if (missing_args == 2 && file->mode == 0)`:
//!   apply the filter list, then `delete_item()` when `statret == 0`.
//! - `flist.c:2254-2258` - `missing_args == 2` sender branch that emits
//!   the mode-0 sentinel this handler consumes.

use std::io;
use std::path::Path;

use logging::{debug_log, info_log};

use crate::receiver::ReceiverContext;

impl ReceiverContext {
    /// Processes mode-0 sentinel entries injected by the sender's
    /// `--delete-missing-args` (`missing_args == 2`) branch.
    ///
    /// For every entry whose `mode == 0` we look up the corresponding
    /// destination path relative to `dest_dir` and remove it if it exists.
    /// Mode-0 entries carry no usable file type bits (the sender writes a
    /// raw zero), so we dispatch on the destination filesystem's symlink
    /// metadata: directories are removed recursively, everything else
    /// is unlinked. Missing destinations are a no-op, matching upstream's
    /// `statret == 0` guard.
    ///
    /// All filesystem mutations route through the sandbox helpers
    /// ([`fast_io::unlink_via_sandbox_or_fallback`],
    /// [`fast_io::recursive_unlinkat_via_sandbox_or_fallback`]) so a TOCTOU
    /// swap on a single-component leaf under the destination root cannot
    /// redirect the deletion to an attacker-chosen path. Multi-component
    /// relative paths take the documented path-based fallback inside the
    /// helper (see `crates/fast_io/src/dir_sandbox/at_syscalls.rs`).
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
    /// - `generator.c:1348-1354` - `missing_args == 2 && file->mode == 0`
    ///   branch that calls `delete_item()` for an existing destination
    ///   and falls through (no creation) for a missing destination.
    pub(in crate::receiver) fn process_missing_args_sentinels(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    ) -> io::Result<()> {
        if !self.config.file_selection.delete_missing_args {
            return Ok(());
        }
        if self.config.flags.dry_run {
            return Ok(());
        }

        for entry in &self.file_list {
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
            let result = if is_dir {
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
