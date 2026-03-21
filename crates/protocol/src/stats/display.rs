//! Display formatting for transfer statistics.
//!
//! Implements `Display` for `TransferStats` to produce output matching
//! upstream rsync's stats format, including comma-separated numbers,
//! file type breakdowns, and speedup calculations.

use super::TransferStats;

impl TransferStats {
    /// Formats a number with comma separators (e.g., 1,234,567).
    pub(crate) fn format_number(n: u64) -> String {
        let s = n.to_string();
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();

        for (i, ch) in chars.iter().enumerate() {
            if i > 0 && (chars.len() - i) % 3 == 0 {
                result.push(',');
            }
            result.push(*ch);
        }

        result
    }

    /// Calculates bytes per second for the transfer.
    fn bytes_per_sec(&self) -> f64 {
        let total_time_secs = (self.flist_buildtime + self.flist_xfertime) as f64 / 1_000_000.0;

        if total_time_secs > 0.0 {
            (self.total_read + self.total_written) as f64 / total_time_secs
        } else {
            0.0
        }
    }

    /// Calculates speedup ratio.
    fn speedup(&self) -> f64 {
        let total_bytes = self.total_read + self.total_written;

        if total_bytes > 0 {
            self.total_size as f64 / total_bytes as f64
        } else {
            0.0
        }
    }
}

impl std::fmt::Display for TransferStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.num_files > 0 {
            write!(
                f,
                "Number of files: {}",
                Self::format_number(self.num_files)
            )?;

            let mut parts = Vec::new();
            if self.num_reg_files > 0 {
                parts.push(format!("reg: {}", Self::format_number(self.num_reg_files)));
            }
            if self.num_dirs > 0 {
                parts.push(format!("dir: {}", Self::format_number(self.num_dirs)));
            }
            if self.num_symlinks > 0 {
                parts.push(format!("link: {}", Self::format_number(self.num_symlinks)));
            }
            if self.num_devices > 0 {
                parts.push(format!("dev: {}", Self::format_number(self.num_devices)));
            }
            if self.num_specials > 0 {
                parts.push(format!(
                    "special: {}",
                    Self::format_number(self.num_specials)
                ));
            }

            if !parts.is_empty() {
                write!(f, " ({})", parts.join(", "))?;
            }
            writeln!(f)?;
        }

        if self.num_created_files > 0 {
            writeln!(
                f,
                "Number of created files: {}",
                Self::format_number(self.num_created_files)
            )?;
        }

        if self.num_deleted_files > 0 {
            writeln!(
                f,
                "Number of deleted files: {}",
                Self::format_number(self.num_deleted_files)
            )?;
        }

        if self.num_transferred_files > 0 {
            writeln!(
                f,
                "Number of regular files transferred: {}",
                Self::format_number(self.num_transferred_files)
            )?;
        }

        if self.total_size > 0 {
            writeln!(
                f,
                "Total file size: {} bytes",
                Self::format_number(self.total_size)
            )?;
        }

        if self.total_transferred_size > 0 {
            writeln!(
                f,
                "Total transferred file size: {} bytes",
                Self::format_number(self.total_transferred_size)
            )?;
        }

        if self.total_transferred_size > 0 || self.literal_data > 0 || self.matched_data > 0 {
            writeln!(
                f,
                "Literal data: {} bytes",
                Self::format_number(self.literal_data)
            )?;
            writeln!(
                f,
                "Matched data: {} bytes",
                Self::format_number(self.matched_data)
            )?;
        }

        if self.flist_size > 0 {
            writeln!(
                f,
                "File list size: {}",
                Self::format_number(self.flist_size)
            )?;
        }

        if self.flist_buildtime > 0 {
            let secs = self.flist_buildtime as f64 / 1_000_000.0;
            writeln!(f, "File list generation time: {secs:.3} seconds")?;
        }

        if self.flist_xfertime > 0 {
            let secs = self.flist_xfertime as f64 / 1_000_000.0;
            writeln!(f, "File list transfer time: {secs:.3} seconds")?;
        }

        writeln!(
            f,
            "Total bytes sent: {}",
            Self::format_number(self.total_written)
        )?;
        writeln!(
            f,
            "Total bytes received: {}",
            Self::format_number(self.total_read)
        )?;

        let bytes_per_sec = self.bytes_per_sec();
        writeln!(
            f,
            "sent {} bytes  received {} bytes  {:.2} bytes/sec",
            Self::format_number(self.total_written),
            Self::format_number(self.total_read),
            bytes_per_sec
        )?;

        let speedup = self.speedup();
        write!(
            f,
            "total size is {}  speedup is {:.2}",
            Self::format_number(self.total_size),
            speedup
        )?;

        Ok(())
    }
}
