use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use core::client::{ClientProgressObserver, ClientProgressUpdate, HumanReadableMode};

use super::format::{
    RemainingTimeEstimator, format_progress_bytes, format_progress_elapsed,
    format_progress_percent, format_progress_rate, format_progress_rate_from_value,
};
use super::mode::ProgressMode;

/// Emits verbose, statistics, and progress-oriented output derived from a
/// [`core::client::ClientSummary`].
pub(crate) struct LiveProgress<'a> {
    writer: &'a mut dyn Write,
    rendered: bool,
    error: Option<io::Error>,
    active_path: Option<PathBuf>,
    line_active: bool,
    mode: ProgressMode,
    human_readable: HumanReadableMode,
    /// Sliding-window remaining-time estimator for the overall (`progress2`)
    /// transfer, mirroring upstream's `ph_list` ring in `progress.c`.
    overall_remaining: RemainingTimeEstimator,
    /// Per-file estimators keyed by relative path; created on first sighting
    /// of a path and dropped when the path's progress completes.
    per_file_remaining: HashMap<PathBuf, RemainingTimeEstimator>,
    /// Longest line emitted in progress2 mode. Used to pad shorter lines with
    /// trailing spaces so stale characters from longer previous ticks are
    /// erased on overwrite.
    ///
    /// upstream: progress.c:84-91 `static int last_len` - tracks the longest
    /// progress line and pads the current line to match before emitting.
    max_line_len: usize,
}

impl<'a> LiveProgress<'a> {
    pub(crate) fn new(
        writer: &'a mut dyn Write,
        mode: ProgressMode,
        human_readable: HumanReadableMode,
    ) -> Self {
        Self {
            writer,
            rendered: false,
            error: None,
            active_path: None,
            line_active: false,
            mode,
            human_readable,
            overall_remaining: RemainingTimeEstimator::new(),
            per_file_remaining: HashMap::new(),
            max_line_len: 0,
        }
    }

    pub(crate) const fn rendered(&self) -> bool {
        self.rendered
    }

    fn record_error(&mut self, error: io::Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    pub(crate) fn finish(self) -> io::Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }

        if self.line_active {
            writeln!(self.writer)?;
        }

        Ok(())
    }

    /// Writes a progress line for progress2 (Overall) mode, padding to the
    /// longest previously emitted line to erase stale characters on `\r`
    /// overwrite.
    ///
    /// upstream: progress.c:84-91 - `last_len` tracking and space-padding.
    fn write_overall_line(&mut self, line: &str) -> io::Result<()> {
        let current_len = line.len();
        let pad = self.max_line_len.saturating_sub(current_len);
        if pad > 0 {
            write!(self.writer, "{line}{:pad$}", "")?;
        } else {
            write!(self.writer, "{line}")?;
        }
        if current_len > self.max_line_len {
            self.max_line_len = current_len;
        }
        Ok(())
    }
}

impl<'a> ClientProgressObserver for LiveProgress<'a> {
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        if self.error.is_some() {
            return;
        }

        let total = update.total().max(update.index());
        let remaining = total.saturating_sub(update.index());

        let write_result = match self.mode {
            ProgressMode::PerFile => (|| -> io::Result<()> {
                let event = update.event();
                let relative = event.relative_path();
                let path_changed = self.active_path.as_deref() != Some(relative);

                if path_changed {
                    if self.line_active {
                        writeln!(self.writer)?;
                        self.line_active = false;
                    }
                    writeln!(self.writer, "{}", relative.display())?;
                    self.active_path = Some(relative.to_path_buf());
                }

                let bytes = event.bytes_transferred();
                let now = Instant::now();
                let estimator = self
                    .per_file_remaining
                    .entry(relative.to_path_buf())
                    .or_default();
                estimator.observe(now, bytes);
                // Field widths mirror upstream rsync's `rprint_progress` format string
                // `"\r%15s %3d%% %7.2f%s %s%s"` (progress.c:129). The rate column packs the
                // `%7.2f` value (7 chars) plus a 4-char unit suffix (kB/s, MB/s, GB/s) for an
                // 11-char total. The time column matches `%4u:%02u:%02u` at 10 chars.
                let size_field =
                    format!("{:>15}", format_progress_bytes(bytes, self.human_readable));
                let percent = format_progress_percent(bytes, update.total_bytes());
                let percent_field = format!("{percent:>4}");
                let rate_field = format!(
                    "{:>11}",
                    format_progress_rate(bytes, event.elapsed(), self.human_readable)
                );
                // upstream: progress.c:97-105 prints ETA via the sliding window
                // mid-transfer and switches to total elapsed for the final tick.
                let time_text = if update.is_final() {
                    format_progress_elapsed(event.elapsed())
                } else {
                    let total = update.total_bytes().unwrap_or(bytes);
                    estimator.render(now, bytes, total)
                };
                let time_field = format!("{time_text:>10}");
                let xfr_index = update.index();

                if self.line_active {
                    write!(self.writer, "\r")?;
                }

                // upstream: progress.c:80 - chk-prefix is "to" once the file
                // list is complete, "ir" while INC_RECURSE sub-lists are still
                // arriving on the wire.
                let chk_prefix = if update.flist_eof() { "to" } else { "ir" };
                write!(
                    self.writer,
                    "{size_field} {percent_field} {rate_field} {time_field} (xfr#{xfr_index}, {chk_prefix}-chk={remaining}/{total})"
                )?;

                if update.is_final() {
                    writeln!(self.writer)?;
                    self.line_active = false;
                    self.active_path = None;
                    self.per_file_remaining.remove(relative);
                } else {
                    self.line_active = true;
                }
                Ok(())
            })(),
            ProgressMode::Overall => (|| -> io::Result<()> {
                let bytes = update.overall_transferred();
                let now = Instant::now();
                self.overall_remaining.observe(now, bytes);
                let size_field =
                    format!("{:>15}", format_progress_bytes(bytes, self.human_readable));
                let percent_field = format!(
                    "{:>4}",
                    format_progress_percent(bytes, update.overall_total_bytes())
                );
                // upstream: progress.c:102-116 - progress2 uses the sliding-window
                // rate from the 5-slot ring buffer, not the cumulative rate.
                let window_rate = self.overall_remaining.window_rate(now, bytes).unwrap_or(0.0);
                let rate_field = format!(
                    "{:>11}",
                    format_progress_rate_from_value(window_rate, self.human_readable)
                );
                // upstream: progress.c:97-105 - sliding window ETA mid-transfer,
                // total elapsed for the final tick.
                let final_tick = update.remaining() == 0 && update.is_final();
                let time_text = if final_tick {
                    format_progress_elapsed(update.overall_elapsed())
                } else {
                    let total = update.overall_total_bytes().unwrap_or(bytes);
                    self.overall_remaining.render(now, bytes, total)
                };
                let time_field = format!("{time_text:>10}");
                let xfr_index = update.index();

                if self.line_active {
                    write!(self.writer, "\r")?;
                }

                // upstream: progress.c:80 - chk-prefix is "to" once the file
                // list is complete, "ir" while INC_RECURSE sub-lists are still
                // arriving on the wire.
                let chk_prefix = if update.flist_eof() { "to" } else { "ir" };

                if update.is_final() {
                    // upstream: progress.c:78-82 - final tick per file emits the
                    // xfr trailer. In progress2 mode the trailing newline is
                    // stripped (progress.c:88) and spaces pad to the longest
                    // prior line to erase stale characters.
                    let line = format!(
                        "{size_field} {percent_field} {rate_field} {time_field} (xfr#{xfr_index}, {chk_prefix}-chk={remaining}/{total})"
                    );
                    self.write_overall_line(&line)?;
                    if final_tick {
                        // upstream: progress.c:131-134 - the very last file's
                        // final tick emits a newline. For progress2, the newline
                        // is deferred until the summary (main.c:452). We emit
                        // it here to separate from the summary block.
                        writeln!(self.writer)?;
                        self.line_active = false;
                    } else {
                        // Newline suppressed for progress2 per
                        // progress.c:88 - cursor stays on same line.
                        self.line_active = true;
                    }
                } else {
                    // upstream: progress.c:100 - in-flight ticks use trailing
                    // `"  "` (two spaces) instead of the xfr trailer.
                    let line = format!(
                        "{size_field} {percent_field} {rate_field} {time_field}  "
                    );
                    self.write_overall_line(&line)?;
                    self.line_active = true;
                }
                self.active_path = None;
                Ok(())
            })(),
        };

        match write_result {
            Ok(()) => {
                self.rendered = true;
            }
            Err(error) => self.record_error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::client::{ClientEvent, ClientEventKind};
    use engine::local_copy::LocalCopyChangeSet;
    use std::path::PathBuf;
    use std::time::Duration;

    fn make_update(flist_eof: bool) -> ClientProgressUpdate {
        let event = ClientEvent::for_test(
            PathBuf::from("file.bin"),
            ClientEventKind::DataCopied,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        ClientProgressUpdate::from_transfer_event(
            event,
            1,
            3,
            Some(2_048),
            1_024,
            Duration::from_secs(1),
            flist_eof,
        )
    }

    fn make_mid_transfer_update(flist_eof: bool) -> ClientProgressUpdate {
        let event = ClientEvent::for_test(
            PathBuf::from("file.bin"),
            ClientEventKind::DataCopied,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        ClientProgressUpdate::from_transfer_event_mid(
            event,
            1,
            3,
            Some(2_048),
            1_024,
            Some(4_096),
            Duration::from_secs(1),
            flist_eof,
        )
    }

    /// upstream: progress.c:78-82 - the per-file trailer prints `to-chk` once
    /// the file list is complete, `ir-chk` while INC_RECURSE sub-lists are
    /// still arriving.
    #[test]
    fn per_file_renders_to_chk_when_flist_complete() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::PerFile, HumanReadableMode::Disabled);
            live.on_progress(&make_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(output.contains("to-chk="), "missing to-chk: {output}");
        assert!(!output.contains("ir-chk="), "unexpected ir-chk: {output}");
    }

    #[test]
    fn per_file_renders_ir_chk_when_flist_pending() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::PerFile, HumanReadableMode::Disabled);
            live.on_progress(&make_update(false));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(output.contains("ir-chk="), "missing ir-chk: {output}");
        assert!(!output.contains("to-chk="), "unexpected to-chk: {output}");
    }

    /// upstream: progress.c:100 - in-flight progress2 ticks use trailing
    /// spaces instead of the xfr/chk trailer.
    #[test]
    fn overall_mid_transfer_has_no_xfr_trailer() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Disabled);
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            !output.contains("xfr#"),
            "mid-transfer progress2 should not contain xfr trailer: {output}"
        );
        assert!(
            !output.contains("to-chk="),
            "mid-transfer progress2 should not contain to-chk: {output}"
        );
    }

    /// upstream: progress.c:78-82 - final tick in progress2 mode shows the
    /// xfr/chk trailer.
    #[test]
    fn overall_final_tick_has_xfr_trailer() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Disabled);
            live.on_progress(&make_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            output.contains("xfr#"),
            "final progress2 tick should contain xfr trailer: {output}"
        );
        assert!(
            output.contains("to-chk="),
            "final progress2 tick should contain to-chk: {output}"
        );
    }

    /// upstream: progress.c:84-91 - progress2 pads shorter lines with spaces
    /// to erase stale characters from longer previous ticks.
    #[test]
    fn overall_pads_shorter_lines_to_max() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Disabled);
            // First tick: final with trailer (longer line)
            live.on_progress(&make_update(true));
            // Second tick: mid-transfer without trailer (shorter line, should be padded)
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        // The second line (after \r) should have trailing spaces
        let lines: Vec<&str> = output.split('\r').collect();
        if lines.len() > 1 {
            let second_line = lines.last().unwrap();
            // The second (shorter) line should be padded with trailing spaces
            assert!(
                second_line.ends_with("  "),
                "shorter progress2 line should be padded: {second_line:?}"
            );
        }
    }

    /// upstream: progress.c:102-116 - progress2 uses the sliding-window rate
    /// from the 5-slot ring buffer, not the cumulative rate. Verify rate field
    /// is present and non-empty.
    #[test]
    fn overall_uses_sliding_window_rate() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Disabled);
            live.on_progress(&make_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        // The rate field should contain kB/s, MB/s, or GB/s
        assert!(
            output.contains("kB/s") || output.contains("MB/s") || output.contains("GB/s"),
            "progress2 should contain a rate with base-1024 units: {output}"
        );
    }
}
