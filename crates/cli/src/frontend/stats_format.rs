//! Statistics formatting module for rsync transfer statistics.
//!
//! This module implements upstream rsync's --stats output format exactly,
//! for displaying transfer statistics at the end of a sync operation.
//!
//! # Overview
//!
//! The module provides a [`StatsFormatter`] that takes a [`StatsData`] struct
//! containing all the transfer statistics and produces a formatted multi-line
//! string matching upstream rsync's exact output format.
//!
//! # Example
//!
//! ```
//! use cli::stats_format::{StatsData, StatsFormatter};
//!
//! let data = StatsData {
//!     num_files: 1234,
//!     num_transferred_files: 42,
//!     total_file_size: 1_234_567,
//!     total_transferred_size: 123_456,
//!     literal_data: 12_345,
//!     matched_data: 111_111,
//!     file_list_size: 1_234,
//!     file_list_generation_time: 0.001,
//!     file_list_transfer_time: 0.0,
//!     total_bytes_sent: 12_345,
//!     total_bytes_received: 67_890,
//!     num_created_files: 56,
//!     num_deleted_files: 0,
//! };
//!
//! let formatter = StatsFormatter::new(data);
//! let output = formatter.format();
//! println!("{}", output);
//! ```

use std::fmt::Write;

/// Statistics data for a transfer operation.
///
/// This struct contains all the data needed to generate upstream-compatible
/// rsync statistics output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatsData {
    /// Number of files in the transfer.
    pub num_files: u64,
    /// Number of regular files transferred.
    pub num_transferred_files: u64,
    /// Total file size in bytes.
    pub total_file_size: u64,
    /// Total transferred file size in bytes.
    pub total_transferred_size: u64,
    /// Literal data bytes (actual data transferred).
    pub literal_data: u64,
    /// Matched data bytes (data reused from destination).
    pub matched_data: u64,
    /// File list size in bytes.
    pub file_list_size: u64,
    /// File list generation time in seconds.
    pub file_list_generation_time: f64,
    /// File list transfer time in seconds.
    pub file_list_transfer_time: f64,
    /// Total bytes sent.
    pub total_bytes_sent: u64,
    /// Total bytes received.
    pub total_bytes_received: u64,
    /// Number of created files.
    pub num_created_files: u64,
    /// Number of deleted files.
    pub num_deleted_files: u64,
}

impl Default for StatsData {
    fn default() -> Self {
        Self {
            num_files: 0,
            num_transferred_files: 0,
            total_file_size: 0,
            total_transferred_size: 0,
            literal_data: 0,
            matched_data: 0,
            file_list_size: 0,
            file_list_generation_time: 0.0,
            file_list_transfer_time: 0.0,
            total_bytes_sent: 0,
            total_bytes_received: 0,
            num_created_files: 0,
            num_deleted_files: 0,
        }
    }
}

/// Formatter for transfer statistics.
///
/// This formatter produces output matching upstream rsync's --stats format exactly.
pub struct StatsFormatter {
    data: StatsData,
}

impl StatsFormatter {
    /// Creates a new statistics formatter with the given data.
    #[must_use]
    pub const fn new(data: StatsData) -> Self {
        Self { data }
    }

    /// Formats the statistics as a multi-line string matching upstream rsync format.
    ///
    /// # Example
    ///
    /// ```
    /// use cli::stats_format::{StatsData, StatsFormatter};
    ///
    /// let data = StatsData {
    ///     num_files: 1234,
    ///     num_transferred_files: 42,
    ///     total_file_size: 1_234_567,
    ///     total_transferred_size: 123_456,
    ///     literal_data: 12_345,
    ///     matched_data: 111_111,
    ///     file_list_size: 1_234,
    ///     file_list_generation_time: 0.001,
    ///     file_list_transfer_time: 0.0,
    ///     total_bytes_sent: 12_345,
    ///     total_bytes_received: 67_890,
    ///     num_created_files: 56,
    ///     num_deleted_files: 0,
    /// };
    ///
    /// let formatter = StatsFormatter::new(data);
    /// let output = formatter.format();
    /// assert!(output.contains("Number of files: 1,234"));
    /// ```
    #[must_use]
    pub fn format(&self) -> String {
        let mut output = String::new();

        // Number of files
        writeln!(
            output,
            "Number of files: {}",
            format_number(self.data.num_files)
        )
        .unwrap();

        // Number of created files
        writeln!(
            output,
            "Number of created files: {}",
            format_number(self.data.num_created_files)
        )
        .unwrap();

        // Number of deleted files
        writeln!(
            output,
            "Number of deleted files: {}",
            format_number(self.data.num_deleted_files)
        )
        .unwrap();

        // Number of regular files transferred
        writeln!(
            output,
            "Number of regular files transferred: {}",
            format_number(self.data.num_transferred_files)
        )
        .unwrap();

        // Total file size
        writeln!(
            output,
            "Total file size: {} bytes",
            format_number(self.data.total_file_size)
        )
        .unwrap();

        // Total transferred file size
        writeln!(
            output,
            "Total transferred file size: {} bytes",
            format_number(self.data.total_transferred_size)
        )
        .unwrap();

        // Literal data
        writeln!(
            output,
            "Literal data: {} bytes",
            format_number(self.data.literal_data)
        )
        .unwrap();

        // Matched data
        writeln!(
            output,
            "Matched data: {} bytes",
            format_number(self.data.matched_data)
        )
        .unwrap();

        // File list size
        writeln!(
            output,
            "File list size: {}",
            format_number(self.data.file_list_size)
        )
        .unwrap();

        // File list generation time
        writeln!(
            output,
            "File list generation time: {:.3} seconds",
            self.data.file_list_generation_time
        )
        .unwrap();

        // File list transfer time
        writeln!(
            output,
            "File list transfer time: {:.3} seconds",
            self.data.file_list_transfer_time
        )
        .unwrap();

        // Total bytes sent
        writeln!(
            output,
            "Total bytes sent: {}",
            format_number(self.data.total_bytes_sent)
        )
        .unwrap();

        // Total bytes received
        writeln!(
            output,
            "Total bytes received: {}",
            format_number(self.data.total_bytes_received)
        )
        .unwrap();

        // Empty line before summary
        writeln!(output).unwrap();

        // Summary line: "sent X bytes  received Y bytes  Z bytes/sec"
        let transfer_speed = calculate_transfer_speed(
            self.data.total_bytes_sent,
            self.data.total_bytes_received,
            self.data.file_list_generation_time + self.data.file_list_transfer_time,
        );

        writeln!(
            output,
            "sent {} bytes  received {} bytes  {} bytes/sec",
            format_number(self.data.total_bytes_sent),
            format_number(self.data.total_bytes_received),
            format_speed(transfer_speed)
        )
        .unwrap();

        // Speedup line: "total size is X  speedup is Y.ZZ"
        let speedup = calculate_speedup(
            self.data.total_file_size,
            self.data.total_bytes_sent,
            self.data.total_bytes_received,
        );

        write!(
            output,
            "total size is {}  speedup is {}",
            format_number(self.data.total_file_size),
            format_speedup(speedup)
        )
        .unwrap();

        output
    }
}

/// Formats a number with thousands separators (commas).
///
/// # Examples
///
/// ```
/// use cli::stats_format::format_number;
///
/// assert_eq!(format_number(0), "0");
/// assert_eq!(format_number(999), "999");
/// assert_eq!(format_number(1000), "1,000");
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

/// Formats a transfer speed with 2 decimal places.
///
/// # Examples
///
/// ```
/// use cli::stats_format::format_speed;
///
/// assert_eq!(format_speed(0.0), "0.00");
/// assert_eq!(format_speed(1234.56), "1,234.56");
/// assert_eq!(format_speed(1234567.89), "1,234,567.89");
/// ```
#[must_use]
pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 0.0 {
        return "0.00".to_string();
    }

    let rounded = (bytes_per_sec * 100.0).round() / 100.0;
    let integer_part = rounded.floor() as u64;
    let fractional_part = ((rounded - integer_part as f64) * 100.0).round() as u64;

    format!("{}.{:02}", format_number(integer_part), fractional_part)
}

/// Formats a speedup ratio with 2 decimal places.
///
/// # Examples
///
/// ```
/// use cli::stats_format::format_speedup;
///
/// assert_eq!(format_speedup(0.0), "0.00");
/// assert_eq!(format_speedup(15.38), "15.38");
/// assert_eq!(format_speedup(1234.567), "1,234.57");
/// ```
#[must_use]
pub fn format_speedup(speedup: f64) -> String {
    if speedup < 0.0 {
        return "0.00".to_string();
    }

    let rounded = (speedup * 100.0).round() / 100.0;
    let integer_part = rounded.floor() as u64;
    let fractional_part = ((rounded - integer_part as f64) * 100.0).round() as u64;

    format!("{}.{:02}", format_number(integer_part), fractional_part)
}

/// Calculates the speedup ratio.
///
/// Speedup is total_size / (sent + received).
fn calculate_speedup(total_size: u64, sent: u64, received: u64) -> f64 {
    let total_transferred = sent + received;
    if total_transferred == 0 {
        return 0.0;
    }
    total_size as f64 / total_transferred as f64
}

/// Calculates the transfer speed in bytes per second.
fn calculate_transfer_speed(sent: u64, received: u64, total_time_secs: f64) -> f64 {
    if total_time_secs <= 0.0 {
        return 0.0;
    }
    (sent + received) as f64 / total_time_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn format_number_no_separator() {
        assert_eq!(format_number(1), "1");
        assert_eq!(format_number(99), "99");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_one_separator() {
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(9999), "9,999");
    }

    #[test]
    fn format_number_two_separators() {
        assert_eq!(format_number(12345), "12,345");
        assert_eq!(format_number(123456), "123,456");
        assert_eq!(format_number(999999), "999,999");
    }

    #[test]
    fn format_number_three_separators() {
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(9999999), "9,999,999");
    }

    #[test]
    fn format_number_large() {
        assert_eq!(format_number(1234567890), "1,234,567,890");
        assert_eq!(format_number(9_999_999_999), "9,999,999,999");
    }

    #[test]
    fn format_speed_zero() {
        assert_eq!(format_speed(0.0), "0.00");
    }

    #[test]
    fn format_speed_small() {
        assert_eq!(format_speed(1.5), "1.50");
        assert_eq!(format_speed(99.99), "99.99");
    }

    #[test]
    fn format_speed_with_separator() {
        assert_eq!(format_speed(1234.56), "1,234.56");
        assert_eq!(format_speed(26745.00), "26,745.00");
    }

    #[test]
    fn format_speed_large() {
        assert_eq!(format_speed(1234567.89), "1,234,567.89");
    }

    #[test]
    fn format_speed_rounds_correctly() {
        assert_eq!(format_speed(1234.567), "1,234.57");
        assert_eq!(format_speed(1234.564), "1,234.56");
    }

    #[test]
    fn format_speed_negative_becomes_zero() {
        assert_eq!(format_speed(-100.0), "0.00");
    }

    #[test]
    fn format_speedup_zero() {
        assert_eq!(format_speedup(0.0), "0.00");
    }

    #[test]
    fn format_speedup_small() {
        assert_eq!(format_speedup(1.5), "1.50");
        assert_eq!(format_speedup(15.38), "15.38");
    }

    #[test]
    fn format_speedup_large() {
        assert_eq!(format_speedup(1234.56), "1,234.56");
    }

    #[test]
    fn format_speedup_rounds_correctly() {
        assert_eq!(format_speedup(1234.567), "1,234.57");
        assert_eq!(format_speedup(1234.564), "1,234.56");
    }

    #[test]
    fn format_speedup_negative_becomes_zero() {
        assert_eq!(format_speedup(-100.0), "0.00");
    }

    #[test]
    fn calculate_speedup_normal() {
        let speedup = calculate_speedup(1_234_567, 12_345, 67_890);
        assert!((speedup - 15.38).abs() < 0.01);
    }

    #[test]
    fn calculate_speedup_zero_transfer() {
        let speedup = calculate_speedup(1_234_567, 0, 0);
        assert_eq!(speedup, 0.0);
    }

    #[test]
    fn calculate_speedup_zero_total_size() {
        let speedup = calculate_speedup(0, 12_345, 67_890);
        assert_eq!(speedup, 0.0);
    }

    #[test]
    fn calculate_transfer_speed_normal() {
        let speed = calculate_transfer_speed(12_345, 67_890, 3.0);
        assert!((speed - 26_745.0).abs() < 1.0);
    }

    #[test]
    fn calculate_transfer_speed_zero_time() {
        let speed = calculate_transfer_speed(12_345, 67_890, 0.0);
        assert_eq!(speed, 0.0);
    }

    #[test]
    fn calculate_transfer_speed_negative_time() {
        let speed = calculate_transfer_speed(12_345, 67_890, -1.0);
        assert_eq!(speed, 0.0);
    }

    #[test]
    fn stats_formatter_full_output() {
        let data = StatsData {
            num_files: 1234,
            num_transferred_files: 42,
            total_file_size: 1_234_567,
            total_transferred_size: 123_456,
            literal_data: 12_345,
            matched_data: 111_111,
            file_list_size: 1_234,
            file_list_generation_time: 0.001,
            file_list_transfer_time: 0.0,
            total_bytes_sent: 12_345,
            total_bytes_received: 67_890,
            num_created_files: 56,
            num_deleted_files: 0,
        };

        let formatter = StatsFormatter::new(data);
        let output = formatter.format();

        assert!(output.contains("Number of files: 1,234"));
        assert!(output.contains("Number of created files: 56"));
        assert!(output.contains("Number of deleted files: 0"));
        assert!(output.contains("Number of regular files transferred: 42"));
        assert!(output.contains("Total file size: 1,234,567 bytes"));
        assert!(output.contains("Total transferred file size: 123,456 bytes"));
        assert!(output.contains("Literal data: 12,345 bytes"));
        assert!(output.contains("Matched data: 111,111 bytes"));
        assert!(output.contains("File list size: 1,234"));
        assert!(output.contains("File list generation time: 0.001 seconds"));
        assert!(output.contains("File list transfer time: 0.000 seconds"));
        assert!(output.contains("Total bytes sent: 12,345"));
        assert!(output.contains("Total bytes received: 67,890"));
        assert!(output.contains("sent 12,345 bytes  received 67,890 bytes"));
        assert!(output.contains("total size is 1,234,567"));
        assert!(output.contains("speedup is"));
    }

    #[test]
    fn stats_formatter_zero_values() {
        let data = StatsData::default();
        let formatter = StatsFormatter::new(data);
        let output = formatter.format();

        assert!(output.contains("Number of files: 0"));
        assert!(output.contains("Number of created files: 0"));
        assert!(output.contains("Number of deleted files: 0"));
        assert!(output.contains("Number of regular files transferred: 0"));
        assert!(output.contains("Total file size: 0 bytes"));
    }

    #[test]
    fn stats_formatter_large_numbers() {
        let data = StatsData {
            num_files: 999_999_999,
            num_transferred_files: 888_888_888,
            total_file_size: 9_999_999_999,
            total_transferred_size: 8_888_888_888,
            literal_data: 7_777_777_777,
            matched_data: 6_666_666_666,
            file_list_size: 5_555_555,
            file_list_generation_time: 123.456,
            file_list_transfer_time: 78.901,
            total_bytes_sent: 4_444_444_444,
            total_bytes_received: 3_333_333_333,
            num_created_files: 222_222_222,
            num_deleted_files: 111_111_111,
        };

        let formatter = StatsFormatter::new(data);
        let output = formatter.format();

        assert!(output.contains("Number of files: 999,999,999"));
        assert!(output.contains("Number of created files: 222,222,222"));
        assert!(output.contains("Number of deleted files: 111,111,111"));
        assert!(output.contains("Total file size: 9,999,999,999 bytes"));
    }

    #[test]
    fn stats_data_default() {
        let data = StatsData::default();
        assert_eq!(data.num_files, 0);
        assert_eq!(data.total_file_size, 0);
        assert_eq!(data.file_list_generation_time, 0.0);
    }

    #[test]
    fn summary_line_format() {
        let data = StatsData {
            total_bytes_sent: 12_345,
            total_bytes_received: 67_890,
            file_list_generation_time: 1.0,
            file_list_transfer_time: 2.0,
            ..Default::default()
        };

        let formatter = StatsFormatter::new(data);
        let output = formatter.format();

        // Should have exactly the upstream format: "sent X bytes  received Y bytes  Z bytes/sec"
        assert!(output.contains("sent 12,345 bytes  received 67,890 bytes"));
    }

    #[test]
    fn speedup_line_format() {
        let data = StatsData {
            total_file_size: 1_234_567,
            total_bytes_sent: 12_345,
            total_bytes_received: 67_890,
            ..Default::default()
        };

        let formatter = StatsFormatter::new(data);
        let output = formatter.format();

        // Should have exactly the upstream format: "total size is X  speedup is Y.ZZ"
        assert!(output.contains("total size is 1,234,567  speedup is"));
    }
}
