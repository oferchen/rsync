//! Progress formatting module for rsync's --progress output.
//!
//! This module implements upstream rsync's progress output formats exactly,
//! including both per-file progress (--progress) and overall progress
//! (--info=progress2).
//!
//! # Overview
//!
//! The module provides two main types:
//! - [`PerFileProgress`]: Displays per-file transfer progress
//! - [`OverallProgress`]: Displays overall transfer progress (--info=progress2)
//!
//! # Example
//!
//! ```
//! use cli::progress_format::{PerFileProgress, OverallProgress};
//! use std::time::Duration;
//!
//! // Per-file progress
//! let mut file_progress = PerFileProgress::new("myfile.txt", 1_234_567);
//! file_progress.update(617_283, Duration::from_secs(5));
//! let line = file_progress.format_line();
//! println!("{}", line);
//!
//! // Overall progress
//! let mut overall = OverallProgress::new(1234, 9_999_999_999);
//! overall.update_file_complete(1_234_567);
//! let line = overall.format_line();
//! println!("{}", line);
//! ```

use std::time::Duration;

/// Per-file transfer progress display.
///
/// Displays progress for a single file transfer in the format:
/// ```text
///   1,234,567 100%   12.34MB/s    0:00:01
/// ```
#[derive(Debug, Clone)]
pub struct PerFileProgress {
    file_name: String,
    file_size: u64,
    bytes_transferred: u64,
    elapsed: Duration,
}

impl PerFileProgress {
    /// Creates a new per-file progress tracker.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::PerFileProgress;
    ///
    /// let progress = PerFileProgress::new("file.txt", 1_000_000);
    /// ```
    #[must_use]
    pub fn new(file_name: &str, file_size: u64) -> Self {
        Self {
            file_name: file_name.to_string(),
            file_size,
            bytes_transferred: 0,
            elapsed: Duration::ZERO,
        }
    }

    /// Updates the progress with the current bytes transferred and elapsed time.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::PerFileProgress;
    /// use std::time::Duration;
    ///
    /// let mut progress = PerFileProgress::new("file.txt", 1_000_000);
    /// progress.update(500_000, Duration::from_secs(5));
    /// ```
    pub fn update(&mut self, bytes_transferred: u64, elapsed: Duration) {
        self.bytes_transferred = bytes_transferred;
        self.elapsed = elapsed;
    }

    /// Formats the current progress as a string.
    ///
    /// Returns a string in the format:
    /// ```text
    ///   1,234,567 100%   12.34MB/s    0:00:01
    /// ```
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::PerFileProgress;
    /// use std::time::Duration;
    ///
    /// let mut progress = PerFileProgress::new("file.txt", 1_000_000);
    /// progress.update(500_000, Duration::from_secs(5));
    /// let line = progress.format_line();
    /// assert!(line.contains("50%"));
    /// ```
    #[must_use]
    pub fn format_line(&self) -> String {
        let bytes_str = format_number(self.bytes_transferred);
        let percent = calculate_percentage(self.bytes_transferred, self.file_size);
        let rate = calculate_rate(self.bytes_transferred, self.elapsed);
        let rate_str = format_rate(rate);
        let elapsed_str = format_elapsed(self.elapsed);

        format!("{bytes_str:>15} {percent:>4}   {rate_str:>12}    {elapsed_str}")
    }

    /// Returns the file name.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// Returns the file size.
    #[must_use]
    pub const fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Returns the bytes transferred so far.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }
}

/// Overall transfer progress display (--info=progress2).
///
/// Displays overall progress across all files in the format:
/// ```text
///    42/1,234 files   3.45% (xfr#42, to-chk=1,192/1,234)
/// ```
#[derive(Debug, Clone)]
pub struct OverallProgress {
    total_files: usize,
    total_size: u64,
    files_completed: usize,
    bytes_completed: u64,
    xfr_number: usize,
}

impl OverallProgress {
    /// Creates a new overall progress tracker.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::OverallProgress;
    ///
    /// let progress = OverallProgress::new(1234, 9_999_999_999);
    /// ```
    #[must_use]
    pub const fn new(total_files: usize, total_size: u64) -> Self {
        Self {
            total_files,
            total_size,
            files_completed: 0,
            bytes_completed: 0,
            xfr_number: 0,
        }
    }

    /// Updates the progress when a file is completed.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::OverallProgress;
    ///
    /// let mut progress = OverallProgress::new(1234, 9_999_999_999);
    /// progress.update_file_complete(1_234_567);
    /// ```
    pub fn update_file_complete(&mut self, file_size: u64) {
        self.files_completed += 1;
        self.bytes_completed += file_size;
        self.xfr_number += 1;
    }

    /// Formats the current progress as a string.
    ///
    /// Returns a string in the format:
    /// ```text
    ///    42/1,234 files   3.45% (xfr#42, to-chk=1,192/1,234)
    /// ```
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::OverallProgress;
    ///
    /// let mut progress = OverallProgress::new(100, 10_000_000);
    /// progress.update_file_complete(100_000);
    /// let line = progress.format_line();
    /// assert!(line.contains("files"));
    /// ```
    #[must_use]
    pub fn format_line(&self) -> String {
        let files_done = self.files_completed;
        let total_files = self.total_files;
        let percent = if self.total_size > 0 {
            let pct = (self.bytes_completed as f64 / self.total_size as f64) * 100.0;
            format!("{pct:>5.2}%")
        } else {
            "  0.00%".to_string()
        };
        let xfr = self.xfr_number;
        let to_chk = total_files.saturating_sub(files_done);

        format!(
            "{files_done:>6}/{} files  {percent} (xfr#{xfr}, to-chk={to_chk}/{})",
            format_number_usize(total_files),
            format_number_usize(total_files)
        )
    }

    /// Returns the number of files completed.
    #[must_use]
    pub const fn files_completed(&self) -> usize {
        self.files_completed
    }

    /// Returns the total number of files.
    #[must_use]
    pub const fn total_files(&self) -> usize {
        self.total_files
    }
}

/// Progress formatter combining both per-file and overall progress.
pub struct ProgressFormatter;

impl ProgressFormatter {
    /// Formats per-file progress with all details.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::ProgressFormatter;
    /// use std::time::Duration;
    ///
    /// let line = ProgressFormatter::format_file_progress(
    ///     "myfile.txt",
    ///     500_000,
    ///     1_000_000,
    ///     100_000.0,
    ///     Duration::from_secs(5)
    /// );
    /// assert!(line.contains("50%"));
    /// ```
    #[must_use]
    pub fn format_file_progress(
        name: &str,
        transferred: u64,
        total: u64,
        _rate: f64,
        eta: Duration,
    ) -> String {
        let mut progress = PerFileProgress::new(name, total);
        progress.update(transferred, eta);
        progress.format_line()
    }

    /// Formats overall progress with all details.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::progress_format::ProgressFormatter;
    ///
    /// let line = ProgressFormatter::format_overall_progress(42, 1234, 42, 1192);
    /// assert!(line.contains("files"));
    /// assert!(line.contains("xfr#42"));
    /// assert!(line.contains("to-chk=1192"));
    /// ```
    #[must_use]
    pub fn format_overall_progress(
        files_done: usize,
        total_files: usize,
        xfr_num: usize,
        to_check: usize,
    ) -> String {
        // Calculate approximate percentage
        let percent = if total_files > 0 {
            let pct = (files_done as f64 / total_files as f64) * 100.0;
            format!("{pct:>5.2}%")
        } else {
            "  0.00%".to_string()
        };

        format!(
            "{files_done:>6}/{} files  {percent} (xfr#{xfr_num}, to-chk={to_check}/{})",
            format_number_usize(total_files),
            format_number_usize(total_files)
        )
    }
}

/// Calculates transfer rate in bytes per second.
///
/// # Example
///
/// ```
/// use cli::progress_format::calculate_rate;
/// use std::time::Duration;
///
/// let rate = calculate_rate(1_000_000, Duration::from_secs(10));
/// assert!((rate - 100_000.0).abs() < 1.0);
/// ```
#[must_use]
pub fn calculate_rate(bytes: u64, elapsed: Duration) -> f64 {
    if elapsed.is_zero() {
        return 0.0;
    }

    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        0.0
    } else {
        bytes as f64 / seconds
    }
}

/// Formats a transfer rate with auto-scaling (B/s, kB/s, MB/s, GB/s).
///
/// Note: Following rsync's behavior, rates are always shown in kB/s or higher.
///
/// # Example
///
/// ```
/// use cli::progress_format::format_rate;
///
/// assert_eq!(format_rate(500.0), "0.49kB/s");
/// assert_eq!(format_rate(1_500.0), "1.46kB/s");
/// assert_eq!(format_rate(1_500_000.0), "1.43MB/s");
/// assert_eq!(format_rate(1_500_000_000.0), "1.40GB/s");
/// ```
#[must_use]
pub fn format_rate(bytes_per_sec: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    if bytes_per_sec >= GIB {
        format!("{:.2}GB/s", bytes_per_sec / GIB)
    } else if bytes_per_sec >= MIB {
        format!("{:.2}MB/s", bytes_per_sec / MIB)
    } else {
        // Always show in kB/s, even for small rates (rsync behavior)
        format!("{:.2}kB/s", bytes_per_sec / KIB)
    }
}

/// Formats an ETA (estimated time to arrival) as H:MM:SS or D:HH:MM:SS.
///
/// # Example
///
/// ```
/// use cli::progress_format::format_eta;
///
/// assert_eq!(format_eta(1_000_000, 100_000.0), "0:00:10");
/// assert_eq!(format_eta(36_000_000, 10_000.0), "1:00:00");
/// ```
#[must_use]
pub fn format_eta(remaining_bytes: u64, rate: f64) -> String {
    if rate <= 0.0 {
        return "0:00:00".to_string();
    }

    let seconds = (remaining_bytes as f64 / rate) as u64;
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;

    if days > 0 {
        format!("{days}:{hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{hours}:{minutes:02}:{secs:02}")
    }
}

/// Formats elapsed time as H:MM:SS.
fn format_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours}:{minutes:02}:{seconds:02}")
}

/// Calculates percentage completion.
fn calculate_percentage(current: u64, total: u64) -> String {
    if total == 0 {
        return "100%".to_string();
    }

    let capped = current.min(total);
    // Use floating point to avoid overflow with large values
    let percent = if total == capped {
        100
    } else {
        ((capped as f64 / total as f64) * 100.0) as u64
    };
    format!("{percent}%")
}

/// Formats a number with thousands separators (commas).
///
/// # Example
///
/// ```
/// use cli::progress_format::format_number;
///
/// assert_eq!(format_number(0), "0");
/// assert_eq!(format_number(1234), "1,234");
/// assert_eq!(format_number(1234567), "1,234,567");
/// ```
#[must_use]
pub fn format_number(n: u64) -> String {
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

/// Formats a usize number with thousands separators (commas).
fn format_number_usize(n: usize) -> String {
    format_number(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // format_number tests
    #[test]
    fn format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn format_number_small() {
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_thousands() {
        assert_eq!(format_number(1_000), "1,000");
        assert_eq!(format_number(1_234), "1,234");
        assert_eq!(format_number(9_999), "9,999");
    }

    #[test]
    fn format_number_millions() {
        assert_eq!(format_number(1_234_567), "1,234,567");
        assert_eq!(format_number(9_999_999), "9,999,999");
    }

    #[test]
    fn format_number_billions() {
        assert_eq!(format_number(1_234_567_890), "1,234,567,890");
    }

    // format_rate tests
    #[test]
    fn format_rate_bytes_per_sec() {
        assert_eq!(format_rate(0.0), "0.00kB/s");
        assert_eq!(format_rate(100.0), "0.10kB/s");
        assert_eq!(format_rate(500.5), "0.49kB/s");
    }

    #[test]
    fn format_rate_kilobytes_per_sec() {
        assert_eq!(format_rate(1_024.0), "1.00kB/s");
        assert_eq!(format_rate(1_536.0), "1.50kB/s");
        assert_eq!(format_rate(10_240.0), "10.00kB/s");
    }

    #[test]
    fn format_rate_megabytes_per_sec() {
        assert_eq!(format_rate(1_048_576.0), "1.00MB/s");
        assert_eq!(format_rate(12_582_912.0), "12.00MB/s");
        assert_eq!(format_rate(1_572_864.0), "1.50MB/s");
    }

    #[test]
    fn format_rate_gigabytes_per_sec() {
        assert_eq!(format_rate(1_073_741_824.0), "1.00GB/s");
        assert_eq!(format_rate(2_147_483_648.0), "2.00GB/s");
    }

    // calculate_rate tests
    #[test]
    fn calculate_rate_zero_elapsed() {
        let rate = calculate_rate(1_000_000, Duration::ZERO);
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn calculate_rate_one_second() {
        let rate = calculate_rate(1_000_000, Duration::from_secs(1));
        assert!((rate - 1_000_000.0).abs() < 1.0);
    }

    #[test]
    fn calculate_rate_fractional_second() {
        let rate = calculate_rate(500_000, Duration::from_millis(500));
        assert!((rate - 1_000_000.0).abs() < 1.0);
    }

    #[test]
    fn calculate_rate_zero_bytes() {
        let rate = calculate_rate(0, Duration::from_secs(10));
        assert_eq!(rate, 0.0);
    }

    // format_eta tests
    #[test]
    fn format_eta_zero_rate() {
        assert_eq!(format_eta(1_000_000, 0.0), "0:00:00");
    }

    #[test]
    fn format_eta_seconds() {
        assert_eq!(format_eta(1_000_000, 100_000.0), "0:00:10");
        assert_eq!(format_eta(5_000_000, 100_000.0), "0:00:50");
    }

    #[test]
    fn format_eta_minutes() {
        assert_eq!(format_eta(6_000_000, 100_000.0), "0:01:00");
        assert_eq!(format_eta(30_000_000, 100_000.0), "0:05:00");
    }

    #[test]
    fn format_eta_hours() {
        assert_eq!(format_eta(360_000_000, 100_000.0), "1:00:00");
        assert_eq!(format_eta(720_000_000, 100_000.0), "2:00:00");
    }

    #[test]
    fn format_eta_days() {
        assert_eq!(format_eta(8_640_000_000, 100_000.0), "1:00:00:00");
        assert_eq!(format_eta(17_280_000_000, 100_000.0), "2:00:00:00");
    }

    // PerFileProgress tests
    #[test]
    fn per_file_progress_new() {
        let progress = PerFileProgress::new("test.txt", 1_000_000);
        assert_eq!(progress.file_name(), "test.txt");
        assert_eq!(progress.file_size(), 1_000_000);
        assert_eq!(progress.bytes_transferred(), 0);
    }

    #[test]
    fn per_file_progress_update() {
        let mut progress = PerFileProgress::new("test.txt", 1_000_000);
        progress.update(500_000, Duration::from_secs(5));
        assert_eq!(progress.bytes_transferred(), 500_000);
    }

    #[test]
    fn per_file_progress_format_zero_percent() {
        let progress = PerFileProgress::new("test.txt", 1_000_000);
        let line = progress.format_line();
        assert!(line.contains("0%"));
    }

    #[test]
    fn per_file_progress_format_fifty_percent() {
        let mut progress = PerFileProgress::new("test.txt", 1_000_000);
        progress.update(500_000, Duration::from_secs(5));
        let line = progress.format_line();
        assert!(line.contains("50%"));
    }

    #[test]
    fn per_file_progress_format_hundred_percent() {
        let mut progress = PerFileProgress::new("test.txt", 1_000_000);
        progress.update(1_000_000, Duration::from_secs(10));
        let line = progress.format_line();
        assert!(line.contains("100%"));
    }

    #[test]
    fn per_file_progress_zero_file_size() {
        let progress = PerFileProgress::new("empty.txt", 0);
        let line = progress.format_line();
        assert!(line.contains("100%"));
    }

    #[test]
    fn per_file_progress_very_large_file() {
        let tb = 1_099_511_627_776_u64; // 1 TiB
        let mut progress = PerFileProgress::new("huge.bin", tb);
        progress.update(tb / 2, Duration::from_secs(100));
        let line = progress.format_line();
        assert!(line.contains("50%"));
    }

    // OverallProgress tests
    #[test]
    fn overall_progress_new() {
        let progress = OverallProgress::new(1234, 9_999_999_999);
        assert_eq!(progress.total_files(), 1234);
        assert_eq!(progress.files_completed(), 0);
    }

    #[test]
    fn overall_progress_update() {
        let mut progress = OverallProgress::new(100, 10_000_000);
        progress.update_file_complete(100_000);
        assert_eq!(progress.files_completed(), 1);
    }

    #[test]
    fn overall_progress_format() {
        let mut progress = OverallProgress::new(1234, 9_999_999_999);
        progress.update_file_complete(345_000_000);
        let line = progress.format_line();
        assert!(line.contains("files"));
        assert!(line.contains("xfr#1"));
        assert!(line.contains("to-chk=1233"));
    }

    #[test]
    fn overall_progress_format_to_chk_count() {
        let mut progress = OverallProgress::new(1234, 9_999_999_999);
        for _ in 0..42 {
            progress.update_file_complete(1_000_000);
        }
        let line = progress.format_line();
        assert!(line.contains("to-chk=1192"));
    }

    #[test]
    fn overall_progress_format_xfr_numbering() {
        let mut progress = OverallProgress::new(100, 10_000_000);
        progress.update_file_complete(100_000);
        let line = progress.format_line();
        assert!(line.contains("xfr#1"));

        progress.update_file_complete(100_000);
        let line = progress.format_line();
        assert!(line.contains("xfr#2"));
    }

    // ProgressFormatter tests
    #[test]
    fn progress_formatter_file_progress() {
        let line = ProgressFormatter::format_file_progress(
            "test.txt",
            500_000,
            1_000_000,
            100_000.0,
            Duration::from_secs(5),
        );
        assert!(line.contains("50%"));
    }

    #[test]
    fn progress_formatter_overall_progress() {
        let line = ProgressFormatter::format_overall_progress(42, 1234, 42, 1192);
        assert!(line.contains("42"));
        assert!(line.contains("1,234"));
        assert!(line.contains("xfr#42"));
        assert!(line.contains("to-chk=1192"));
    }

    // Additional edge case tests
    #[test]
    fn format_number_u64_max() {
        let max = u64::MAX;
        let formatted = format_number(max);
        assert!(formatted.contains(','));
    }

    #[test]
    fn calculate_percentage_overflow_protection() {
        let percent = calculate_percentage(u64::MAX, u64::MAX);
        assert_eq!(percent, "100%");
    }

    #[test]
    fn format_elapsed_zero() {
        let elapsed = format_elapsed(Duration::ZERO);
        assert_eq!(elapsed, "0:00:00");
    }

    #[test]
    fn format_elapsed_hours() {
        let elapsed = format_elapsed(Duration::from_secs(3661));
        assert_eq!(elapsed, "1:01:01");
    }
}
