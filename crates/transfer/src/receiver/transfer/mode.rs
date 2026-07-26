//! Shared receiver drive-mode selection and the non-transfer drive bodies.
//!
//! Every receiver driver (`run_pipelined`, `run_pipelined_incremental`) picks
//! exactly one of a fixed set of mutually exclusive modes. The choice lives here
//! once, in [`ReceiverContext::select_mode`], and every mode that drives no file
//! data is *implemented* here once, in
//! [`ReceiverContext::run_non_transfer_mode`]. A driver may only supply its own
//! body for [`ReceiverMode::Transfer`].
//!
//! This shape is deliberate. The drivers previously each open-coded an
//! `if list_only { } else if dry_run { } else { }` ladder, and a mode added to
//! one ladder was silently missing from the other - the shipped binary and the
//! CI feature set take different drivers, so the divergence was invisible to
//! both. Now a new [`ReceiverMode`] variant fails to compile in every driver
//! that does not handle it, and a new [`NonTransferMode`] variant fails to
//! compile until its single shared body exists.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use protocol::flist::FileEntry;

use crate::receiver::stats::TransferStats;
use crate::receiver::{PipelineSetup, ReceiverContext};

use super::candidates::new_dir_count;

/// A receiver mode that moves no file data.
///
/// Each variant has exactly one implementation, in
/// [`ReceiverContext::run_non_transfer_mode`], so every driver gets a new mode
/// the moment it is added.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::receiver) enum NonTransferMode {
    /// `--list-only`: render every flist entry, send no per-file request.
    ListOnly,
    /// `--only-write-batch`: real block checksums on the wire, nothing written
    /// to the destination.
    OnlyWriteBatch,
    /// `--dry-run`: itemize and tally every entry, request no file data.
    DryRun,
}

/// Which drive mode a receiver run takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::receiver) enum ReceiverMode {
    /// A mode with no file data, driven by the one shared body.
    NonTransfer(NonTransferMode),
    /// The full delta pipeline - the only mode whose body is driver-specific.
    Transfer,
}

impl ReceiverContext {
    /// Picks the one mode this run drives.
    ///
    /// The ordering is upstream's and is load-bearing:
    ///
    /// - `--list-only` first (`generator.c:1249`): it renders entries via
    ///   `list_file_entry()` and sends no per-file NDX at all. It does *not*
    ///   set `dry_run`, but checking it first keeps that independent of the
    ///   flag's future wiring.
    /// - `--only-write-batch` before `--dry-run` (`main.c:1839`): `write_batch
    ///   < 0` forces `dry_run = 1` while leaving `do_xfers = 1`, so the flag
    ///   pair is ambiguous and only the order disambiguates it. The generator
    ///   still sends real block checksums and the sender expects a sum head per
    ///   file (`sender.c:442-443`); taking the dry-run body here would send a
    ///   bare NDX + iflags with no sum head and hang both ends.
    /// - `--dry-run` last (`generator.c:1858-1959`): NDX + iflags, no sum head.
    pub(in crate::receiver) const fn select_mode(&self) -> ReceiverMode {
        if self.config.flags.list_only {
            ReceiverMode::NonTransfer(NonTransferMode::ListOnly)
        } else if self.config.flags.only_write_batch {
            ReceiverMode::NonTransfer(NonTransferMode::OnlyWriteBatch)
        } else if self.config.flags.dry_run {
            ReceiverMode::NonTransfer(NonTransferMode::DryRun)
        } else {
            ReceiverMode::Transfer
        }
    }

    /// Drives one non-transfer mode to completion.
    ///
    /// Returns `(files_transferred, transferred_file_size)` - the tallies
    /// upstream still reports for a run that moves no data (`receiver.c:781-784`
    /// bumps `stats.xferred_files` before the `if (!do_xfers)` continue).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1249` - `--list-only` renders entries, sends no request.
    /// - `main.c:1839` - `if (write_batch < 0) dry_run = 1`, `do_xfers` stays 1.
    /// - `generator.c:1858-1959` - the `!do_xfers` dry-run request shape.
    pub(in crate::receiver) fn run_non_transfer_mode<
        'a,
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &'a self,
        mode: NonTransferMode,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        setup: &PipelineSetup,
        files_to_transfer: &[(usize, &'a FileEntry, PathBuf, u32)],
        stats: &mut TransferStats,
    ) -> io::Result<(usize, u64)> {
        match mode {
            NonTransferMode::ListOnly => {
                stats.list_only_entries = self.collect_list_only_entries();
                writer.flush()?;
                Ok((0, 0))
            }
            NonTransferMode::OnlyWriteBatch => {
                // The same reporting pass the plain dry run uses; only the wire
                // loop differs (real block checksums, no plan). A push receiver
                // reads no delta - the client sender diverted it into its own
                // batch fd (sender.c:217) - while a pull receiver drains the
                // remote sender's delta via discard_receive_data()
                // (receiver.c:813-814).
                let plan = self.plan_dry_run(&setup.dest_dir, files_to_transfer);
                stats.directories_created = new_dir_count(&plan);
                self.run_only_write_batch_loop(reader, writer, files_to_transfer, setup)?;
                Ok((0, 0))
            }
            NonTransferMode::DryRun => {
                // upstream: recv_generator() itemizes every entry and the
                // receiver tallies every ITEM_IS_NEW even under --dry-run (only
                // the data transfer and the filesystem mutation are skipped).
                // The directory, symlink, and candidate passes early-return
                // under skip_dest_writes(), so plan_dry_run is the one place the
                // rows and created-file counts are produced.
                let plan = self.plan_dry_run(&setup.dest_dir, files_to_transfer);
                stats.directories_created = new_dir_count(&plan);
                self.run_dry_run_loop(reader, writer, &plan)
            }
        }
    }
}
