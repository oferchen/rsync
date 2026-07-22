use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use core::client::{ClientProgressObserver, ClientProgressUpdate, HumanReadableMode};

use super::format::{
    RemainingTimeEstimator, format_progress_bytes, format_progress_elapsed,
    format_progress_percent, format_progress_rate, format_progress_rate_from_value,
};
use super::mode::ProgressMode;
use crate::frontend::outbuf::OutbufMode;

/// Minimum interval between rendered in-flight progress ticks.
///
/// upstream: progress.c:224 `show_progress` returns early when less than
/// 1000ms have elapsed since the last recorded tick, so the overall progress
/// line refreshes at most once per second. `end_progress` (the xfr-trailer
/// final tick) bypasses this throttle.
const TICK_INTERVAL: Duration = Duration::from_millis(1_000);

/// Decides whether an in-flight progress tick should be suppressed.
///
/// Mirrors upstream `show_progress`'s throttle: an in-flight tick is dropped
/// when less than `interval` has elapsed since the previous rendered tick.
/// The first tick (`last` is `None`) and every final tick (`is_final`, the
/// xfr-trailer emitted by `end_progress`) always render.
///
/// upstream: progress.c:224 - `if (msdiff(&ph_list[newest_hpos].time, &now) < 1000) return;`
fn tick_throttled(last: Option<Instant>, now: Instant, interval: Duration, is_final: bool) -> bool {
    if is_final {
        return false;
    }
    match last {
        Some(prev) => now.saturating_duration_since(prev) < interval,
        None => false,
    }
}

/// Controls how progress output adapts to terminal vs piped destinations
/// and how buffering interacts with progress ticks.
///
/// upstream: progress.c uses `\r` for in-place overwrite on terminals.
/// When piped, progress lines would overwrite each other invisibly, so
/// upstream's `output_needs_newline` mechanism ensures a `\n` is emitted
/// before any non-progress message. We go further: when the output is not
/// a terminal, we use `\n` instead of `\r` so piped output is readable.
///
/// upstream: options.c:2030-2052 `--outbuf` controls stdout buffering via
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
    /// Longest xfr-trailer line emitted in progress2 mode. Only the final
    /// (trailer) ticks are padded to this width so a shorter trailer erases
    /// stale characters from a longer previous trailer. In-flight ticks are
    /// never padded, matching upstream (their `"  "` end-of-line leaves the
    /// previous trailer's trailing characters on screen).
    ///
    /// upstream: progress.c:84-91 `static int last_len` - tracks the longest
    /// trailer (the `is_last` eol) and pads it before emitting.
    max_trailer_len: usize,
    /// Timestamp of the last rendered in-flight tick, used to throttle the
    /// overall progress line to one refresh per [`TICK_INTERVAL`].
    ///
    /// upstream: progress.c:224 `ph_list[newest_hpos].time`.
    last_tick: Option<Instant>,
    /// Minimum interval between rendered in-flight ticks. Defaults to
    /// [`TICK_INTERVAL`]; overridable in tests for deterministic rendering.
    tick_interval: Duration,
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

    /// Builds a live progress renderer bound to `writer`, using the given
    /// progress mode, human-readable level, and terminal/buffering config.
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
            max_trailer_len: 0,
            last_tick: None,
            tick_interval: TICK_INTERVAL,
            output_config,
        }
    }

    /// Returns whether at least one progress line has been rendered.
    pub(crate) const fn rendered(&self) -> bool {
        self.rendered
    }

    fn record_error(&mut self, error: io::Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// Finalizes progress output, terminating any active line with a newline
    /// and surfacing the first I/O error recorded during rendering.
    pub(crate) fn finish(self) -> io::Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }

        if self.line_active {
            writeln!(self.writer)?;
        }

        Ok(())
    }

    /// Writes the line prefix that returns the cursor to column 0 before a
    /// progress tick.
    ///
    /// On a terminal, upstream prefixes *every* progress line with `\r` -
    /// including the first - so the cursor is always at column 0 before the
    /// fields are written (progress.c:129 `"\r%15s ..."`). We emit the `\r`
    /// unconditionally in terminal mode to match that byte stream.
    ///
    /// When piped, `\r` would be invisible and successive ticks would smear
    /// onto one line, so we emit a `\n` *between* lines only (never before the
    /// first) to keep piped output human-readable. This is an oc-rsync
    /// readability extension; the terminal path stays byte-faithful.
    fn write_line_restart(&mut self) -> io::Result<()> {
        if self.output_config.is_terminal {
            write!(self.writer, "\r")
        } else if self.line_active {
            writeln!(self.writer)
        } else {
            Ok(())
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

    /// Writes a final (xfr-trailer) progress2 line, padding it with trailing
    /// spaces to the longest trailer emitted so far so a shorter trailer
    /// erases stale characters from a longer previous one on `\r` overwrite.
    ///
    /// Only trailer lines are padded. Upstream tracks `last_len` for the
    /// `is_last` eol alone; in-flight ticks (their `"  "` eol) are written
    /// verbatim and deliberately leave the previous trailer's trailing
    /// characters on screen.
    ///
    /// upstream: progress.c:84-91 - `last_len` tracking and space-padding.
    fn write_padded_trailer(&mut self, line: &str) -> io::Result<()> {
        let current_len = line.len();
        // Only pad when output goes to a terminal - padding erases stale
        // characters from a longer previous `\r`-overwritten trailer. When
        // piped, each tick is on its own line so padding is unnecessary.
        if self.output_config.is_terminal {
            let pad = self.max_trailer_len.saturating_sub(current_len);
            if pad > 0 {
                write!(self.writer, "{line}{:pad$}", "")?;
            } else {
                write!(self.writer, "{line}")?;
            }
        } else {
            write!(self.writer, "{line}")?;
        }
        if current_len > self.max_trailer_len {
            self.max_trailer_len = current_len;
        }
        Ok(())
    }
}

impl<'a> ClientProgressObserver for LiveProgress<'a> {
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        if self.error.is_some() {
            return;
        }

        // upstream: receiver.c:786-788 - the NDX_DONE end_progress(0) summary
        // is emitted only under --info=progress2. Per-file progress mode has no
        // terminal summary line, so drop the synthetic transfer-complete tick.
        if update.is_transfer_complete() && matches!(self.mode, ProgressMode::PerFile) {
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
                let now = Instant::now();
                let is_final = update.is_final();

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
                    // upstream: progress.c:205-222 - show_progress seeds
                    // ph_start to the file's transfer start, so a new file's
                    // throttle baseline is its start rather than a "first tick
                    // always renders" special-case. A fast single-chunk file
                    // whose intermediate tick falls within one interval of its
                    // start therefore renders only the final xfr line.
                    self.last_tick = Some(now);
                }

                // upstream: progress.c:224 - show_progress renders an in-flight
                // tick at most once per interval since the last render, while
                // end_progress (the final xfr-trailer tick) bypasses the
                // throttle. Without this the forwarder's intermediate
                // handle_progress tick and the final handle tick both render,
                // duplicating the 100% line on non-terminal (piped) output.
                if tick_throttled(self.last_tick, now, self.tick_interval, is_final) {
                    return Ok(());
                }

                let bytes = event.bytes_transferred();
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
                let time_text = if is_final {
                    format_progress_elapsed(event.elapsed())
                } else {
                    let total = update.total_bytes().unwrap_or(bytes);
                    estimator.render(now, bytes, total)
                };
                let time_field = format!("{time_text:>10}");

                if self.line_active {
                    if self.output_config.is_terminal {
                        write!(self.writer, "\r")?;
                    } else {
                        writeln!(self.writer)?;
                    }
                }

                if is_final {
                    // upstream: progress.c:77-92 - the `(xfr#..)` trailer is
                    // emitted only on the final (is_last) tick; per-file mode
                    // keeps the trailing newline. progress.c:80 - chk-prefix is
                    // "to" once the file list is complete, "ir" while
                    // INC_RECURSE sub-lists are still arriving on the wire.
                    let chk_prefix = if update.flist_eof() { "to" } else { "ir" };
                    let xfr_index = update.index();
                    write!(
                        self.writer,
                        "{size_field} {percent_field} {rate_field} {time_field} (xfr#{xfr_index}, {chk_prefix}-chk={remaining}/{total})"
                    )?;
                    writeln!(self.writer)?;
                    self.line_active = false;
                    self.active_path = None;
                    self.per_file_remaining.remove(relative);
                } else {
                    // upstream: progress.c:99-100 - in-flight ticks use a
                    // trailing `"  "` (two spaces) instead of the xfr trailer.
                    write!(
                        self.writer,
                        "{size_field} {percent_field} {rate_field} {time_field}  "
                    )?;
                    self.line_active = true;
                    // upstream: progress.c:224 - the throttle baseline advances
                    // to the last rendered in-flight tick; the final tick does
                    // not advance it.
                    self.last_tick = Some(now);
                    // upstream: progress.c:133 - rflush(FCLIENT) after
                    // every non-final progress tick.
                    self.flush_if_needed()?;
                }
                Ok(())
            })(),
            ProgressMode::Overall => (|| -> io::Result<()> {
                let now = Instant::now();
                let is_final = update.is_final();
                // upstream: progress.c:224 - show_progress() refreshes the
                // overall line at most once per second; end_progress() (the
                // xfr-trailer final tick) bypasses the throttle so every file
                // boundary renders. Without this, a per-chunk event stream
                // would emit hundreds of ticks per file instead of ~1/sec.
                if tick_throttled(self.last_tick, now, self.tick_interval, is_final) {
                    return Ok(());
                }

                let bytes = update.overall_transferred();
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
                let final_tick = update.remaining() == 0 && is_final;
                let time_text = if final_tick {
                    format_progress_elapsed(update.overall_elapsed())
                } else {
                    let total = update.overall_total_bytes().unwrap_or(bytes);
                    self.overall_remaining.render(now, bytes, total)
                };
                let time_field = format!("{time_text:>10}");
                let xfr_index = update.index();

                // upstream: progress.c:129 - `"\r%15s ..."` prefixes every line
                // (including the first) with a carriage return on a terminal.
                self.write_line_restart()?;

                // upstream: progress.c:80 - chk-prefix is "to" once the file
                // list is complete, "ir" while INC_RECURSE sub-lists are still
                // arriving on the wire.
                let chk_prefix = if update.flist_eof() { "to" } else { "ir" };

                if is_final {
                    // upstream: progress.c:78-82 - final tick per file emits the
                    // xfr trailer. In progress2 mode the trailing newline is
                    // stripped (progress.c:88) and spaces pad the trailer to the
                    // longest prior trailer to erase stale characters.
                    let line = format!(
                        "{size_field} {percent_field} {rate_field} {time_field} (xfr#{xfr_index}, {chk_prefix}-chk={remaining}/{total})"
                    );
                    self.write_padded_trailer(&line)?;
                    if final_tick {
                        // upstream: progress.c:131-134 - the very last file's
                        // final tick emits a newline. For progress2, the newline
                        // is deferred until the summary (main.c:461). We emit
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
                    // upstream: progress.c:100 - in-flight ticks use a trailing
                    // `"  "` (two spaces) and are NOT padded, so a shorter tick
                    // leaves the previous trailer's trailing characters on
                    // screen exactly as upstream does.
                    write!(
                        self.writer,
                        "{size_field} {percent_field} {rate_field} {time_field}  "
                    )?;
                    self.line_active = true;
                    // upstream: progress.c:224 - the throttle baseline is the
                    // last in-flight tick; final ticks do not advance it.
                    self.last_tick = Some(now);
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

    /// Builds a per-path in-flight (non-final) update.
    fn make_mid_for(path: &str) -> ClientProgressUpdate {
        let event = ClientEvent::for_test(
            PathBuf::from(path),
            ClientEventKind::DataCopied,
            false,
            None,
            LocalCopyChangeSet::new(),
        );
        ClientProgressUpdate::from_transfer_event_mid(
            event,
            1,
            2,
            Some(2_048),
            1_024,
            Some(2_048),
            Duration::from_secs(0),
            true,
        )
    }

    /// Builds a per-path final update carrying the xfr trailer.
    fn make_final_for(path: &str, index: usize) -> ClientProgressUpdate {
        let event = ClientEvent::for_test(
            PathBuf::from(path),
            ClientEventKind::DataCopied,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        ClientProgressUpdate::from_transfer_event(
            event,
            index,
            2,
            Some(2_048),
            2_048,
            Duration::from_secs(0),
            true,
        )
    }

    /// upstream: progress.c:77-92, 224 - a local copy forwards an intermediate
    /// `handle_progress` tick and a final `handle` tick per file. When both
    /// fall within one throttle interval (a fast single-chunk file), only the
    /// final tick renders, and the `(xfr#..)` trailer is emitted solely on
    /// that final tick. On non-terminal (piped) output each rendered tick is a
    /// separate line, so exactly one `xfr#` line must appear per file - never
    /// two. This is the regression guard for the duplicate-line bug.
    #[test]
    fn per_file_piped_emits_one_xfr_trailer_per_file() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::PerFile,
                HumanReadableMode::Grouped,
                piped_config(),
            );
            // Two files, each an intermediate tick immediately followed by its
            // final tick (well within the default throttle interval).
            live.on_progress(&make_mid_for("a.bin"));
            live.on_progress(&make_final_for("a.bin", 1));
            live.on_progress(&make_mid_for("b.bin"));
            live.on_progress(&make_final_for("b.bin", 2));
        }
        let output = String::from_utf8(buf).expect("utf8");
        let xfr_lines = output.lines().filter(|line| line.contains("xfr#")).count();
        assert_eq!(
            xfr_lines, 2,
            "expected exactly one xfr trailer per file: {output:?}"
        );
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

    /// Builds a final (trailer) update with an explicit `xfr#`/`to-chk`
    /// denominator so a test can vary the trailer length.
    fn make_final_update(index: usize, total: usize) -> ClientProgressUpdate {
        let event = ClientEvent::for_test(
            PathBuf::from("file.bin"),
            ClientEventKind::DataCopied,
            true,
            None,
            LocalCopyChangeSet::new(),
        );
        ClientProgressUpdate::from_transfer_event(
            event,
            index,
            total,
            Some(0),
            0,
            Duration::from_secs(1),
            true,
        )
    }

    /// upstream: progress.c:99-100 - an in-flight progress2 tick uses a `"  "`
    /// end-of-line and is NOT padded. A shorter in-flight tick following a
    /// longer xfr trailer must leave the trailer's trailing characters on
    /// screen exactly as upstream does, so it carries only the 2-space eol.
    #[test]
    fn overall_does_not_pad_in_flight_ticks() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Grouped);
            // Final trailer first (a long line), then an in-flight tick.
            live.on_progress(&make_final_update(1, 1_000));
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        // Terminal mode: every line is `\r`-prefixed; the last segment is the
        // in-flight tick. Its width is the fixed 45-column layout
        // (15 + 1 + 4 + 1 + 11 + 1 + 10 + 2), never padded up to the trailer.
        let last = output.rsplit('\r').next().unwrap();
        assert!(
            last.ends_with("  "),
            "in-flight eol should be 2 spaces: {last:?}"
        );
        assert_eq!(
            last.len(),
            45,
            "in-flight tick must not be padded to the trailer width: {last:?}"
        );
    }

    /// upstream: progress.c:84-91 - only the xfr trailer (the `is_last` eol) is
    /// padded, to the longest trailer seen, so a shorter trailer erases stale
    /// characters from a longer previous one.
    #[test]
    fn overall_pads_trailer_to_longest() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live =
                LiveProgress::new(&mut buf, ProgressMode::Overall, HumanReadableMode::Grouped);
            // Long trailer first (to-chk=999/1000), then a short one (to-chk=X/3).
            live.on_progress(&make_final_update(1, 1_000));
            live.on_progress(&make_final_update(1, 3));
        }
        let output = String::from_utf8(buf).expect("utf8");
        let segments: Vec<&str> = output.split('\r').filter(|s| !s.is_empty()).collect();
        assert_eq!(segments.len(), 2, "expected two trailer lines: {output:?}");
        assert_eq!(
            segments[0].len(),
            segments[1].len(),
            "shorter trailer must be padded to the longer one: {segments:?}"
        );
        assert!(
            segments[1].ends_with(' '),
            "padded trailer should end with spaces: {:?}",
            segments[1]
        );
    }

    /// upstream: progress.c:129 - the progress2 line format begins with `\r`,
    /// so the very first tick on a terminal is `\r`-prefixed (not bare).
    #[test]
    fn overall_first_terminal_line_starts_with_cr() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                terminal_config(),
            );
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            output.starts_with('\r'),
            "first terminal progress2 line must start with \\r: {output:?}"
        );
    }

    /// upstream: progress.c:224 - `show_progress` refreshes the overall line at
    /// most once per second; end_progress (the final tick) bypasses it.
    #[test]
    fn tick_throttle_matches_upstream_one_per_second() {
        let interval = Duration::from_millis(1_000);
        let t0 = Instant::now();
        // First tick (no prior) always renders.
        assert!(!tick_throttled(None, t0, interval, false));
        // A second in-flight tick within 1s is suppressed.
        let t_early = t0 + Duration::from_millis(500);
        assert!(tick_throttled(Some(t0), t_early, interval, false));
        // After 1s it renders again.
        let t_late = t0 + Duration::from_millis(1_000);
        assert!(!tick_throttled(Some(t0), t_late, interval, false));
        // A final (xfr-trailer) tick always renders, even within 1s.
        assert!(!tick_throttled(Some(t0), t_early, interval, true));
    }

    /// The observer suppresses rapid in-flight ticks: after a rendered tick,
    /// a second in-flight tick under the throttle interval produces no output.
    #[test]
    fn overall_throttles_rapid_in_flight_ticks() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                terminal_config(),
            );
            // Two back-to-back in-flight ticks (well under 1s apart in a test).
            live.on_progress(&make_mid_transfer_update(true));
            live.on_progress(&make_mid_transfer_update(true));
        }
        let output = String::from_utf8(buf).expect("utf8");
        // Only the first in-flight tick renders; the second is throttled.
        assert_eq!(
            output.matches('\r').count(),
            1,
            "second rapid in-flight tick should be throttled: {output:?}"
        );
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
            // Disable throttling so both in-flight ticks render and exercise
            // the `\n`-vs-`\r` line separator choice under test.
            live.tick_interval = Duration::ZERO;
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
            // Disable throttling so both in-flight ticks render and exercise
            // the `\r` in-place overwrite under test.
            live.tick_interval = Duration::ZERO;
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

    /// upstream: progress.c:129 - on a terminal every line is `\r`-prefixed,
    /// including the first (when `line_active` is false).
    #[test]
    fn write_line_restart_terminal_always_cr() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                terminal_config(),
            );
            // First line: line_active is false, still emits \r.
            live.write_line_restart().unwrap();
            live.line_active = true;
            live.write_line_restart().unwrap();
        }
        assert_eq!(buf, b"\r\r");
    }

    /// When piped, a `\n` is emitted only *between* lines, never before the
    /// first, so piped progress output has no leading blank line.
    #[test]
    fn write_line_restart_piped_newline_between_lines_only() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut live = LiveProgress::with_output_config(
                &mut buf,
                ProgressMode::Overall,
                HumanReadableMode::Grouped,
                piped_config(),
            );
            // First line: line_active is false, emits nothing.
            live.write_line_restart().unwrap();
            live.line_active = true;
            // Between lines: emits a single newline.
            live.write_line_restart().unwrap();
        }
        assert_eq!(buf, b"\n");
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
            // Disable throttling so the single in-flight tick renders and
            // triggers the flush under test.
            live.tick_interval = Duration::ZERO;
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
            // Disable throttling so the in-flight tick renders; block-buffered
            // mode must still not flush.
            live.tick_interval = Duration::ZERO;
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
            // Disable throttling so the single in-flight tick renders and
            // triggers the flush under test.
            live.tick_interval = Duration::ZERO;
            live.on_progress(&make_mid_transfer_update(true));
        }
        let count = writer.flush_count.load(Ordering::Relaxed);
        assert!(
            count > 0,
            "line-buffered outbuf should flush after non-final ticks, got {count} flushes"
        );
    }
}
