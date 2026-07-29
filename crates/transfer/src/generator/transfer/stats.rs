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

use super::super::GeneratorContext;

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
        total_read: u64,
        total_written: u64,
        flist_buildtime_ms: u64,
        flist_xfertime_ms: u64,
    ) -> io::Result<()> {
        // upstream: flist.c:690-691 - stats.total_size accumulates F_LENGTH for
        // regular files and symlinks only, tallied in send_file_entry() as each
        // entry is written. Read that running total rather than summing
        // `self.file_list` (which INC_RECURSE has drained down to the final
        // sub-list, and which would also count directory sizes upstream omits).
        let total_size: u64 = self.flist_send_stats.total_size;

        // upstream: main.c:349-350 - handle_stats() writes the sender's cached raw
        // descriptor counters (total_read, total_written; io.c:820/859) as the
        // first two varlong30 values, sampled at the caller's handle_stats point.
        let stats = TransferStats::with_bytes(total_read, total_written, total_size)
            .with_flist_times(flist_buildtime_ms, flist_xfertime_ms);

        stats.write_to(writer, self.protocol)?;
        writer.flush()?;
        Ok(())
    }

    /// Writes the `--write-batch` stats trailer into the batch file.
    ///
    /// A client sender puts nothing on the wire here, so the same five
    /// varlong30 values a server sender would send are written straight to the
    /// batch instead. No-op unless this process is a client sender recording a
    /// batch (`batch_stats_sink` is `None` on a pull, where the trailer arrives
    /// over the wire and the read tee already captured it).
    ///
    /// Position matters as much as content: `--read-batch` reads the five
    /// values and then expects the goodbye `NDX_DONE`, so the trailer has to be
    /// written before the goodbye exchange tees that byte, not after the
    /// transfer returns.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:374-383` - `handle_stats()` `else if (write_batch)` arm
    /// - `main.c:1345-1347` - `handle_stats(-1)` then `read_final_goodbye()`
    pub(super) fn record_batch_stats(
        &self,
        total_read: u64,
        total_written: u64,
        flist_buildtime_ms: u64,
        flist_xfertime_ms: u64,
    ) -> io::Result<()> {
        let Some(sink) = self.batch_stats_sink.as_ref() else {
            return Ok(());
        };

        // upstream: main.c:377-378 - the client sender's --write-batch trailer
        // records the same cached raw descriptor counters it would send on the
        // wire, so --read-batch reproduces the identical "Total bytes" report.
        let stats =
            TransferStats::with_bytes(total_read, total_written, self.flist_send_stats.total_size)
                .with_flist_times(flist_buildtime_ms, flist_xfertime_ms);

        let mut guard = sink
            .0
            .lock()
            .map_err(|_| io::Error::other("batch recorder lock poisoned"))?;
        let mut batch: &mut dyn Write = &mut *guard;
        stats.write_to(&mut batch, self.protocol)?;
        batch.flush()
    }
}

#[cfg(test)]
mod tests {
    use crate::config::ServerConfig;
    use crate::flags::{NumericIds, ParsedServerFlags};
    use crate::generator::GeneratorContext;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use protocol::{ProtocolVersion, TransferStats};

    fn ctx() -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            flags: ParsedServerFlags {
                numeric_ids: NumericIds::Explicit,
                ..Default::default()
            },
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        GeneratorContext::new_for_test(&handshake, config)
    }

    /// `send_stats` transmits the raw wire totals it is handed, verbatim.
    ///
    /// WHY: upstream's `handle_stats()` writes the sender's cached raw descriptor
    /// counters `stats.total_read`/`stats.total_written` (main.c:349-350,
    /// io.c:820/859), not any logical delta/token tally. The orchestrator samples
    /// those raw counters at the `handle_stats` point and passes them in; this
    /// pins that they reach the wire unchanged so a pulling client reports the
    /// exact byte totals the sender observed.
    #[test]
    fn send_stats_transmits_the_raw_totals_verbatim() {
        let ctx = ctx();
        let total_read = 96u64;
        let total_written = 9_780u64;

        let mut got = Vec::new();
        ctx.send_stats(&mut got, total_read, total_written, 0, 0)
            .unwrap();

        // The wire must be exactly the (total_read, total_written, total_size)
        // trailer - total_size is the flist tally, 0 for an unpopulated context.
        let mut want = Vec::new();
        TransferStats::with_bytes(total_read, total_written, ctx.flist_send_stats.total_size)
            .with_flist_times(0, 0)
            .write_to(&mut want, ctx.protocol)
            .unwrap();

        assert_eq!(got, want);
    }
}
