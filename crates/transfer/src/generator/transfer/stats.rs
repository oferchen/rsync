//! Transfer statistics emission for the generator role.
//!
//! Contains `send_stats`, which writes total_read/total_written/total_size
//! plus flist build/xfer times to the client after the transfer loop ends.
//!
//! # Upstream Reference
//!
//! - `main.c:347-357` - `handle_stats()` server-sender write path

use std::io::{self, Write};

use protocol::TransferStats;

use super::super::{GeneratorContext, TransferLoopResult};

impl GeneratorContext {
    /// Sends transfer statistics to the client after the transfer loop completes.
    ///
    /// Only called in server mode (daemon sender). Writes total_read,
    /// total_written, total_size as varlong30 values, plus flist_buildtime
    /// and flist_xfertime for protocol >= 29.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:347-357` - `handle_stats()` server-sender write path
    /// - `main.c:978-980` - `do_server_sender()` calls `handle_stats(f_out)`
    pub(super) fn send_stats<W: Write>(
        &self,
        writer: &mut W,
        transfer_result: &TransferLoopResult,
        flist_buildtime_ms: u64,
        flist_xfertime_ms: u64,
    ) -> io::Result<()> {
        // upstream: flist.c:690-691 - stats.total_size accumulates F_LENGTH for
        // regular files and symlinks only, tallied in send_file_entry() as each
        // entry is written. Read that running total rather than summing
        // `self.file_list` (which INC_RECURSE has drained down to the final
        // sub-list, and which would also count directory sizes upstream omits).
        let total_size: u64 = self.flist_send_stats.total_size;

        let stats = TransferStats::with_bytes(
            self.timing.total_bytes_read,
            transfer_result.bytes_sent,
            total_size,
        )
        .with_flist_times(flist_buildtime_ms, flist_xfertime_ms);

        stats.write_to(writer, self.protocol)?;
        writer.flush()?;
        Ok(())
    }
}
