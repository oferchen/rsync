//! Transfer orchestration for the receiver role.
//!
//! Provides the `run`, `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! entry points plus the common `setup_transfer` initialization. The driving
//! loops live in their own submodules:
//!
//! - [`sync`] - sequential per-file transfer used by `run_sync`.
//! - [`pipelined`] - decoupled two-phase pipeline used by `run_pipelined`.
//! - [`pipelined_incremental`] - same as `pipelined` plus incremental directory
//!   creation and failed-dir tracking.
//! - [`setup`] - common multiplex/filter/file-list setup.
//! - [`phases`] - protocol phase exchange and goodbye handshake.
//! - [`candidates`] - candidate-file selection for the pipelined paths.
//! - [`pipeline`] - the inner `run_pipeline_loop_decoupled` plus dry-run loop.

mod candidates;
mod phases;
mod pipeline;
mod pipelined;
mod pipelined_incremental;
mod setup;
mod sync;

use std::io::{self, Read, Write};

use crate::receiver::ReceiverContext;
use crate::receiver::stats::TransferStats;

impl ReceiverContext {
    /// Runs the receiver role to completion.
    ///
    /// Orchestrates the full receive operation: file list reception, signature
    /// generation, delta application, and metadata finalization. Delegates to
    /// `run_pipelined_incremental` (with `incremental-flist`) or `run_pipelined`.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` main reception loop
    /// - `main.c:1160-1200` - `do_recv()` orchestration
    pub fn run<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental(
                reader,
                writer,
                crate::pipeline::PipelineConfig::default(),
                progress,
            )
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            let _ = progress;
            self.run_pipelined(reader, writer, crate::pipeline::PipelineConfig::default())
        }
    }
}
