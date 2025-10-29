use std::io::{self, Write};
use std::path::PathBuf;

use rsync_core::client::{ClientProgressObserver, ClientProgressUpdate, HumanReadableMode};

use super::format::{
    format_progress_bytes, format_progress_elapsed, format_progress_percent, format_progress_rate,
};
use super::mode::ProgressMode;

/// Emits verbose, statistics, and progress-oriented output derived from a
/// [`rsync_core::client::ClientSummary`].
pub(crate) struct LiveProgress<'a> {
    writer: &'a mut dyn Write,
    rendered: bool,
    error: Option<io::Error>,
    active_path: Option<PathBuf>,
    line_active: bool,
    mode: ProgressMode,
    human_readable: HumanReadableMode,
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
        }
    }

    pub(crate) fn rendered(&self) -> bool {
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
                let size_field =
                    format!("{:>15}", format_progress_bytes(bytes, self.human_readable));
                let percent = format_progress_percent(bytes, update.total_bytes());
                let percent_field = format!("{:>4}", percent);
                let rate_field = format!(
                    "{:>12}",
                    format_progress_rate(bytes, event.elapsed(), self.human_readable)
                );
                let elapsed_field = format!("{:>11}", format_progress_elapsed(event.elapsed()));
                let xfr_index = update.index();

                if self.line_active {
                    write!(self.writer, "\r")?;
                }

                write!(
                    self.writer,
                    "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
                )?;

                if update.is_final() {
                    writeln!(self.writer)?;
                    self.line_active = false;
                    self.active_path = None;
                } else {
                    self.line_active = true;
                }
                Ok(())
            })(),
            ProgressMode::Overall => (|| -> io::Result<()> {
                let bytes = update.overall_transferred();
                let size_field =
                    format!("{:>15}", format_progress_bytes(bytes, self.human_readable));
                let percent_field = format!(
                    "{:>4}",
                    format_progress_percent(bytes, update.overall_total_bytes())
                );
                let rate_field = format!(
                    "{:>12}",
                    format_progress_rate(bytes, update.overall_elapsed(), self.human_readable)
                );
                let elapsed_field =
                    format!("{:>11}", format_progress_elapsed(update.overall_elapsed()));
                let xfr_index = update.index();

                if self.line_active {
                    write!(self.writer, "\r")?;
                }

                write!(
                    self.writer,
                    "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
                )?;

                if update.remaining() == 0 && update.is_final() {
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
