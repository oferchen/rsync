//! Receiver-side handler for `--delete-missing-args` mode-0 sentinel entries.
//!
//! When the sender emits a mode-0 sentinel entry for a vanished top-level
//! source (`flist.c:2951-2955`), the receiver must delete the corresponding
//! destination path if it exists and skip any further processing for that
//! entry. Without this handler the sentinel survives in the file list but
//! triggers no filesystem action, leaving stale destination state behind
//! and producing the observable "missing-arg file was not deleted"
//! divergence against upstream rsync.
//!
//! # Upstream Reference
//!
//! - `generator.c:1749-1755` - `if (missing_args == 2 && file->mode == 0)`:
//!   apply the filter list, then `delete_item()` when `statret == 0`.
//! - `flist.c:2951-2955` `send_file_list()` - `missing_args == 2` branch that emits
//!   the mode-0 sentinel this handler consumes.

use std::io;
use std::path::Path;

use crate::receiver::ReceiverContext;

impl ReceiverContext {
    /// Processes mode-0 sentinel entries injected by the sender's
    /// `--delete-missing-args` (`missing_args == 2`) branch.
    ///
    /// Every entry whose `mode == 0` names a destination path to delete. The
    /// sender writes a raw zero, so the type comes from the destination's own
    /// `lstat`, and the removal is upstream's `delete_item()`: a directory is
    /// emptied first only under `DEL_RECURSE` (`--delete` or `--force`),
    /// otherwise a populated one is kept with `cannot delete non-empty
    /// directory: %s`; each removal is itemized, counted and capped by
    /// `--max-delete`. A missing destination is a no-op, matching upstream's
    /// `statret == 0` guard.
    ///
    /// `--dry-run` reports the same rows without removing anything, as
    /// upstream's `do_unlink()` / `do_rmdir()` do. `--list-only` does nothing.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1786-1792` - `missing_args == 2 && file->mode == 0`
    ///   branch that calls `delete_item()` for an existing destination
    ///   and falls through (no creation) for a missing destination.
    /// - `generator.c:1666` - `del_opts` carries `DEL_RECURSE` only for
    ///   `delete_mode || force_delete`.
    /// - `delete.c:113-118`, `delete.c:176-179` - without `DEL_RECURSE` a
    ///   populated directory is `DR_NOT_EMPTY`, reported at `FINFO` and kept.
    /// - `syscall.c:848-850`, `syscall.c:1598-1600` - the unlink and rmdir
    ///   succeed without acting under `dry_run`.
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
    /// handles each sentinel inline in `recv_generator()` (generator.c:1786-1792).
    pub(in crate::receiver) fn process_missing_args_sentinels_in_range<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        range: std::ops::Range<usize>,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) -> io::Result<()> {
        if !self.config.file_selection.delete_missing_args || self.config.flags.list_only {
            return Ok(());
        }
        // upstream: generator.c:1786 - the sentinel is identified by mode == 0.
        // The implicit root entry is never one to act on.
        let mut sentinels = self.file_list[range]
            .iter()
            .filter(|entry| entry.mode() == 0)
            .map(|entry| entry.path().as_path())
            .filter(|name| !name.as_os_str().is_empty() && name.as_os_str() != ".")
            .peekable();
        if sentinels.peek().is_none() {
            return Ok(());
        }
        self.delete_named_entries(
            dest_dir,
            #[cfg(unix)]
            sandbox,
            sentinels,
            writer,
        )
    }
}
