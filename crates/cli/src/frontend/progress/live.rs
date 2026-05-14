use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use core::client::{ClientProgressObserver, ClientProgressUpdate, HumanReadableMode};

use super::format::{
    RemainingTimeEstimator, format_progress_bytes, format_progress_elapsed,
    format_progress_percent, format_progress_rate,
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

                write!(
                    self.writer,
                    "{size_field} {percent_field} {rate_field} {time_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
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
                let rate_field = format!(
                    "{:>11}",
                    format_progress_rate(bytes, update.overall_elapsed(), self.human_readable)
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

                write!(
                    self.writer,
                    "{size_field} {percent_field} {rate_field} {time_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
                )?;

                if final_tick {
                    writeln!(self.writer)?;
                    self.line_active = false;
                } else {
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
