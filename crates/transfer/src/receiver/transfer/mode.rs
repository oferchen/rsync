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
use std::ops::Range;
use std::path::PathBuf;

use protocol::codec::{MonotonicNdxWriter, NdxCodecEnum, create_ndx_codec};

use crate::receiver::stats::TransferStats;
use crate::receiver::{PipelineSetup, ReceiverContext};

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
    /// - `--only-write-batch` before `--dry-run` (`main.c:1866`): `write_batch
    ///   < 0` forces `dry_run = 1` while leaving `do_xfers = 1`, so the flag
    ///   pair is ambiguous and only the order disambiguates it. The generator
    ///   still sends real block checksums and the sender expects a sum head per
    ///   file (`sender.c:768-769`); taking the dry-run body here would send a
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

    /// Drives one non-transfer mode to completion over the whole file list.
    ///
    /// Returns `(files_transferred, transferred_file_size)` - the tallies
    /// upstream still reports for a run that moves no data (`receiver.c:797-800`
    /// bumps `stats.xferred_files` before the `if (!do_xfers)` continue).
    ///
    /// The batch drivers call this once with the list fully received; the
    /// INC_RECURSE streaming driver calls
    /// [`run_non_transfer_segment`](Self::run_non_transfer_segment) per
    /// sub-list instead.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1249` - `--list-only` renders entries, sends no request.
    /// - `main.c:1866` - `if (write_batch < 0) dry_run = 1`, `do_xfers` stays 1.
    /// - `generator.c:1858-1959` - the `!do_xfers` dry-run request shape.
    pub(in crate::receiver) fn run_non_transfer_mode<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &mut self,
        mode: NonTransferMode,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        setup: &PipelineSetup,
        files_to_transfer: &[(usize, PathBuf, u32)],
        stats: &mut TransferStats,
    ) -> io::Result<(usize, u64)> {
        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());
        self.run_non_transfer_segment(
            mode,
            0..self.file_list.len(),
            reader,
            writer,
            setup,
            files_to_transfer,
            stats,
            &mut ndx_write_codec,
            &mut ndx_read_codec,
        )
    }

    /// Puts a `--list-only` listing, collected in flat-index order from index 0,
    /// into the order upstream's generator lists it.
    ///
    /// upstream: generator.c:1638-1644 - `list_file_entry()` runs as
    /// recv_generator() reaches each entry, so under INC_RECURSE a directory is
    /// listed at the head of its own sub-list, not inside its parent's.
    pub(in crate::receiver) fn order_list_only_entries(&self, stats: &mut TransferStats) {
        let entries = std::mem::take(&mut stats.list_only_entries);
        stats.list_only_entries = self
            .in_generator_walk_order(entries.into_iter().enumerate())
            .collect();
    }

    /// Drives one non-transfer mode over the flat-index range `range`.
    ///
    /// `files_to_transfer` must be the candidate list built for the same range.
    /// The NDX codec pair is the connection-wide read/write state (io.c keeps a
    /// single `prev_positive`/`prev_negative` per direction), so the streaming
    /// driver threads one pair through every segment. Running the ranges of a
    /// list in order produces the same rows, tallies, and request bytes as one
    /// whole-list call; the tallies are accumulated into `stats`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2803-2820` - generate_files() runs recv_generator() over
    ///   one sub-list at a time, whatever the mode.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::receiver) fn run_non_transfer_segment<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &mut self,
        mode: NonTransferMode,
        range: Range<usize>,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        setup: &PipelineSetup,
        files_to_transfer: &[(usize, PathBuf, u32)],
        stats: &mut TransferStats,
        ndx_write_codec: &mut MonotonicNdxWriter,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<(usize, u64)> {
        match mode {
            NonTransferMode::ListOnly => {
                let entries = self.collect_list_only_entries_in_range(range);
                stats.list_only_entries.extend(entries);
                writer.flush()?;
                Ok((0, 0))
            }
            NonTransferMode::OnlyWriteBatch => {
                // The same reporting pass the plain dry run uses; only the wire
                // loop differs (real block checksums, no plan). A push receiver
                // reads no delta - the client sender diverted it into its own
                // batch fd (sender.c:220) - while a pull receiver drains the
                // remote sender's delta via discard_receive_data()
                // (receiver.c:829-830).
                let plan = self.plan_dry_run_in_range(range, &setup.dest_dir, files_to_transfer);
                stats.directories_created += self.new_dir_count(&plan);
                self.run_only_write_batch_loop(
                    reader,
                    writer,
                    files_to_transfer,
                    setup,
                    ndx_write_codec,
                    ndx_read_codec,
                )?;
                Ok((0, 0))
            }
            NonTransferMode::DryRun => {
                // upstream: recv_generator() itemizes every entry and the
                // receiver tallies every ITEM_IS_NEW even under --dry-run (only
                // the data transfer and the filesystem mutation are skipped).
                // The directory, symlink, and candidate passes early-return
                // under skip_dest_writes(), so plan_dry_run_in_range is the one place the
                // rows and created-file counts are produced.
                let plan = self.plan_dry_run_in_range(range, &setup.dest_dir, files_to_transfer);
                stats.directories_created += self.new_dir_count(&plan);
                self.run_dry_run_loop(reader, writer, &plan, ndx_write_codec, ndx_read_codec)
            }
        }
    }
}
