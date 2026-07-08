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
use crate::frontend::outbuf::OutbufMode;

/// Controls how progress output adapts to terminal vs piped destinations
/// and how buffering interacts with progress ticks.
///
/// upstream: progress.c uses `\r` for in-place overwrite on terminals.
/// When piped, progress lines would overwrite each other invisibly, so
/// upstream's `output_needs_newline` mechanism ensures a `\n` is emitted
/// before any non-progress message. We go further: when the output is not
/// a terminal, we use `\n` instead of `\r` so piped output is readable.
///
/// upstream: options.c:2012-2034 `--outbuf` controls stdout buffering via
/// `setvbuf`. We mirror this by flushing after each progress tick when
/// the mode is unbuffered or line-buffered.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProgressOutputConfig {
    /// Whether the output destination is an interactive terminal.
    pub(crate) is_terminal: bool,
    /// The `--outbuf` buffering mode, if specified.
    pub(crate) outbuf_mode: Option<OutbufMode>,
}

impl Default for ProgressOutputConfig {
    fn default() -> Self {
        Self {
            is_terminal: true,
            outbuf_mode: None,
        }
    }
}

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
    /// Terminal and buffering configuration for progress output.
    output_config: ProgressOutputConfig,
}

impl<'a> LiveProgress<'a> {
    #[cfg(test)]
    pub(crate) fn new(
        writer: &'a mut dyn Write,
        mode: ProgressMode,
        human_readable: HumanReadableMode,
    ) -> Self {
        Self::with_output_config(
            writer,
            mode,
            human_readable,
            ProgressOutputConfig::default(),
        )
    }

    pub(crate) fn with_output_config(
        writer: &'a mut dyn Write,
        mode: ProgressMode,
        human_readable: HumanReadableMode,
        output_config: ProgressOutputConfig,
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
            output_config,
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

    /// Returns the carriage-return prefix used to overwrite the current line.
    ///
    /// When the output is a terminal, returns `\r` for in-place overwrite.
    /// When piped, returns `\n` so each progress tick appears on its own line.
    ///
    /// upstream: progress.c:129 uses `\r` unconditionally because upstream
    /// relies on `output_needs_newline` + terminal process group checks in
    /// `show_progress` to suppress output when backgrounded or piped.
    fn line_restart(&self) -> &'static str {
        if self.output_config.is_terminal {
            "\r"
        } else {
            "\n"
        }
    }

    /// Flushes the writer after a progress tick if the outbuf mode requires it.
    ///
    /// upstream: progress.c:133 calls `rflush(FCLIENT)` after every non-final
    /// progress tick. We mirror this for unbuffered and line-buffered modes.
    /// Block-buffered mode defers flushing to the OS or explicit flush calls.
    fn flush_if_needed(&mut self) -> io::Result<()> {
        match self.output_config.outbuf_mode {
            Some(OutbufMode::None) => self.writer.flush(),
            Some(OutbufMode::Line) => self.writer.flush(),
            Some(OutbufMode::Block) | None => Ok(()),
        }
    }

    /// Writes a progress line for progress2 (Overall) mode, padding to the
    /// longest previously emitted line to erase stale characters on `\r`
    /// overwrite.
    ///
    /// upstream: progress.c:84-91 - `last_len` tracking and space-padding.
    fn write_overall_line(&mut self, line: &str) -> io::Result<()> {
        let current_len = line.len();
        // Only pad when output goes to a terminal - padding erases stale
        // characters from longer previous `\r`-overwritten ticks. When piped,
        // each tick is on its own line so padding is unnecessary.
        if self.output_config.is_terminal {
            let pad = self.max_line_len.saturating_sub(current_len);
            if pad > 0 {
                write!(self.writer, "{line}{:pad$}", "")?;
            } else {
                write!(self.writer, "{line}")?;
            }
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
        // Use the transfer-relative remaining computed by the forwarder rather
        // than `total - index`: `total` counts every checked file-list entry
        // (the to-chk denominator) while `index` is the transfer ordinal, so
        // `total - index` never reaches 0 when up-to-date entries (which are
        // counted in `total` but never transferred) trail the transfers. The
        // forwarder's `remaining` counts down the transfers themselves, so
        // to-chk reaches 0 on the last one. For remote transfers
        // (`from_transfer_event`) this equals the old `total - index`.
        let remaining = update.remaining().min(total);

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
                    // upstream: flist.c f_name() emits POSIX forward-slash
                    // separators regardless of host OS. Normalize Windows
                    // native backslashes at the rendering boundary.
                    let name = relative.display().to_string();
                    #[cfg(windows)]
                    let name = name.replace('\\', "/");
                    writeln!(self.writer, "{name}")?;
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
                    write!(self.writer, "{}", self.line_restart())?;
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
                    // upstream: progress.c:133 - rflush(FCLIENT) after
                    // every non-final progress tick.
                    self.flush_if_needed()?;
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
                let window_rate = self
                    .overall_remaining
                    .window_rate(now, bytes)
                    .unwrap_or(0.0);
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
                    write!(self.writer, "{}", self.line_restart())?;
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
                        // upstream: progress.c:133 - rflush(FCLIENT)
                        self.flush_if_needed()?;
                    }
                } else {
                    // upstream: progress.c:100 - in-flight ticks use trailing
                    // `"  "` (two spaces) instead of the xfr trailer.
                    let line = format!("{size_field} {percent_field} {rate_field} {time_field}  ");
                    self.write_overall_line(&line)?;
                    self.line_active = true;
                    // upstream: progress.c:133 - rflush(FCLIENT) after
                    // every non-final progress tick.
                    self.flush_if_needed()?;
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
                LiveProgress::new(&mut buf, ProgressMode::PerFile, HumanReadableMode::Grouped);
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
                LiveProgress::new(&mut buf, ProgressMode::PerFile, HumanReadableMode::Grouped);
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
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Grouped);
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
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Grouped);
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
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Grouped);
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
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Grouped);
            live.on_progress(&make_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        // The rate field should contain kB/s, MB/s, or GB/s
        assert!(
            output.contains("kB/s") || output.contains("MB/s") || output.contains("GB/s"),
            "progress2 should contain a rate with base-1024 units: {output}"
        );
    }

    fn piped_config() -> ProgressOutputConfig {
        ProgressOutputConfig {
            is_terminal: false,
            outbuf_mode: None,
        }
    }

    fn terminal_config() -> ProgressOutputConfig {
        ProgressOutputConfig {
            is_terminal: true,
            outbuf_mode: None,
        }
    }

    /// When output is not a terminal, progress lines should use `\n` instead
    /// of `\r` so piped output is human-readable.
    #[test]
    fn per_file_uses_newline_when_piped() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::PerFile,
                HumanReadableMode::Grouped,
                piped_config(),
            );
            live.on_progress(&make_mid_transfer_update(true));
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            !output.contains('\r'),
            "piped per-file output should not contain \\r: {output:?}"
        );
    }

    /// When output is a terminal, progress lines should use `\r` for in-place
    /// overwrite, matching upstream rsync behaviour.
    #[test]
    fn per_file_uses_cr_when_terminal() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::PerFile,
                HumanReadableMode::Grouped,
                terminal_config(),
            );
            live.on_progress(&make_mid_transfer_update(true));
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            output.contains('\r'),
            "terminal per-file output should contain \\r: {output:?}"
        );
    }

    /// When output is not a terminal, overall (progress2) mode should use
    /// `\n` instead of `\r`.
    #[test]
    fn overall_uses_newline_when_piped() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                piped_config(),
            );
            live.on_progress(&make_update(true));
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            !output.contains('\r'),
            "piped progress2 output should not contain \\r: {output:?}"
        );
    }

    /// When output is a terminal, overall mode uses `\r` for overwrite.
    #[test]
    fn overall_uses_cr_when_terminal() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                terminal_config(),
            );
            live.on_progress(&make_update(true));
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            output.contains('\r'),
            "terminal progress2 output should contain \\r: {output:?}"
        );
    }

    /// When piped, overall mode should not pad shorter lines since each line
    /// appears on its own row and there are no stale characters to erase.
    #[test]
    fn overall_no_padding_when_piped() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                piped_config(),
            );
            // First tick: final with trailer (longer line)
            live.on_progress(&make_update(true));
            // Second tick: mid-transfer without trailer (shorter line)
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        // Split on \n and look at the mid-transfer line (the last non-empty one)
        let lines: Vec<&str> = output.lines().collect();
        if let Some(last) = lines.last() {
            // The mid-transfer line ends with "  " (two trailing spaces from
            // upstream format), but should NOT have extra padding spaces beyond
            // that since we're not in terminal mode.
            let content = last.trim_start();
            // Count trailing spaces beyond the two-space upstream suffix
            let trimmed = content.trim_end();
            let trailing_spaces = content.len() - trimmed.len();
            assert!(
                trailing_spaces <= 2,
                "piped output should not pad beyond the 2-space suffix: {last:?} ({trailing_spaces} trailing spaces)"
            );
        }
    }

    /// Verify that `ProgressOutputConfig::default()` produces terminal mode
    /// for backward compatibility with existing callers.
    #[test]
    fn default_output_config_is_terminal() {
        let config = ProgressOutputConfig::default();
        assert!(
            config.is_terminal,
            "default config should assume terminal output"
        );
        assert!(
            config.outbuf_mode.is_none(),
            "default config should have no outbuf mode"
        );
    }

    /// Verify that `line_restart()` returns the correct character for each mode.
    #[test]
    fn line_restart_terminal_vs_piped() {
        let mut buf: Vec<u8> = Vec::new();

        let live_term = LiveProgress::with_output_config(
            &mut buf,
            ProgressMode::PerFile,
            HumanReadableMode::Grouped,
            terminal_config(),
        );
        assert_eq!(live_term.line_restart(), "\r");

        let live_pipe = LiveProgress::with_output_config(
            &mut buf,
            ProgressMode::PerFile,
            HumanReadableMode::Grouped,
            piped_config(),
        );
        assert_eq!(live_pipe.line_restart(), "\n");
    }

    /// Flush is called for unbuffered outbuf mode on non-final ticks.
    #[test]
    fn flush_triggered_for_unbuffered_outbuf() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FlushCountingWriter {
            inner: Vec<u8>,
            flush_count: AtomicUsize,
        }
        impl Write for FlushCountingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flush_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        let mut writer = FlushCountingWriter {
            inner: Vec::new(),
            flush_count: AtomicUsize::new(0),
        };
        {
            let config = ProgressOutputConfig {
                is_terminal: true,
                outbuf_mode: Some(OutbufMode::None),
            };
            let mut live = LiveProgress::with_output_config(
                &mut writer,
                ProgressMode::PerFile,
                HumanReadableMode::Grouped,
                config,
            );
            // Mid-transfer tick should trigger flush
            live.on_progress(&make_mid_transfer_update(true));
        }
        let count = writer.flush_count.load(Ordering::Relaxed);
        assert!(
            count > 0,
            "unbuffered outbuf should flush after non-final ticks, got {count} flushes"
        );
    }

    /// Flush is NOT called for block-buffered outbuf mode.
    #[test]
    fn no_flush_for_block_buffered_outbuf() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FlushCountingWriter {
            inner: Vec<u8>,
            flush_count: AtomicUsize,
        }
        impl Write for FlushCountingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flush_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        let mut writer = FlushCountingWriter {
            inner: Vec::new(),
            flush_count: AtomicUsize::new(0),
        };
        {
            let config = ProgressOutputConfig {
                is_terminal: true,
                outbuf_mode: Some(OutbufMode::Block),
            };
            let mut live = LiveProgress::with_output_config(
                &mut writer,
                ProgressMode::PerFile,
                HumanReadableMode::Grouped,
                config,
            );
            live.on_progress(&make_mid_transfer_update(true));
        }
        let count = writer.flush_count.load(Ordering::Relaxed);
        assert_eq!(
            count, 0,
            "block-buffered outbuf should not flush on progress ticks, got {count} flushes"
        );
    }

    /// Line-buffered outbuf mode should flush after non-final ticks.
    #[test]
    fn flush_triggered_for_line_buffered_outbuf() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FlushCountingWriter {
            inner: Vec<u8>,
            flush_count: AtomicUsize,
        }
        impl Write for FlushCountingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flush_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        let mut writer = FlushCountingWriter {
            inner: Vec::new(),
            flush_count: AtomicUsize::new(0),
        };
        {
            let config = ProgressOutputConfig {
                is_terminal: true,
                outbuf_mode: Some(OutbufMode::Line),
            };
            let mut live = LiveProgress::with_output_config(
                &mut writer,
                ProgressMode::PerFile,
                HumanReadableMode::Grouped,
                config,
            );
            live.on_progress(&make_mid_transfer_update(true));
        }
        let count = writer.flush_count.load(Ordering::Relaxed);
        assert!(
            count > 0,
            "line-buffered outbuf should flush after non-final ticks, got {count} flushes"
        );
    }
}
