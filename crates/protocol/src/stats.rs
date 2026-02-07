//! crates/protocol/src/stats.rs
//!
//! Transfer statistics wire format encoding and decoding.
//!
//! This module implements the wire format for exchanging transfer statistics
//! between rsync processes. The format varies by protocol version:
//!
//! - Protocol 30+: Uses varlong30 encoding with 3-byte minimum
//! - Protocol 29: Adds flist build/transfer time fields
//! - Protocol < 29: Basic stats only
//!
//! # Wire Format
//!
//! Stats are exchanged at the end of a transfer in this order:
//!
//! ```text
//! total_read      : varlong30 (bytes read by sender, written by receiver)
//! total_written   : varlong30 (bytes written by sender, read by receiver)
//! total_size      : varlong30 (total file size)
//! flist_buildtime : varlong30 (protocol >= 29, microseconds)
//! flist_xfertime  : varlong30 (protocol >= 29, microseconds)
//! ```
//!
//! Note: The meaning of read/write swaps between sender and receiver perspectives.

use std::io::{self, Read, Write};

use crate::varint::{read_varlong30, write_varlong30};
use crate::version::ProtocolVersion;

/// Transfer statistics exchanged between rsync processes.
///
/// These statistics are sent at the end of a transfer to allow both sides
/// to report accurate totals. The field meanings depend on the role:
///
/// - **Sender**: `total_read` is bytes received, `total_written` is bytes sent
/// - **Receiver**: `total_read` is bytes sent, `total_written` is bytes received
///
/// # Wire Format
///
/// The core byte counts (`total_read`, `total_written`, `total_size`) and
/// optional timing fields are serialized using `varlong30` encoding. Use
/// [`write_to`](Self::write_to) and [`read_from`](Self::read_from) for
/// wire-format I/O.
///
/// # Examples
///
/// ```
/// use protocol::TransferStats;
///
/// let stats = TransferStats::with_bytes(1024, 2048, 10000)
///     .with_flist_times(500_000, 100_000);
///
/// assert_eq!(stats.total_read, 1024);
/// assert_eq!(stats.total_written, 2048);
/// assert_eq!(stats.total_size, 10000);
///
/// // Swap perspective when relaying stats between sender and receiver
/// let swapped = stats.swap_perspective();
/// assert_eq!(swapped.total_read, 2048);
/// assert_eq!(swapped.total_written, 1024);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransferStats {
    /// Total bytes read from the network.
    pub total_read: u64,
    /// Total bytes written to the network.
    pub total_written: u64,
    /// Total size of all files in the transfer.
    pub total_size: u64,
    /// Time spent building the file list (microseconds).
    pub flist_buildtime: u64,
    /// Time spent transferring the file list (microseconds).
    pub flist_xfertime: u64,
    /// Total entries received from wire (incremental mode).
    pub entries_received: u64,
    /// Directories successfully created (incremental mode).
    pub directories_created: u64,
    /// Directories that failed to create (incremental mode).
    pub directories_failed: u64,
    /// Files skipped due to failed parent directory (incremental mode).
    pub files_skipped: u64,
    /// Symlinks created (incremental mode).
    pub symlinks_created: u64,
    /// Special files created (incremental mode).
    pub specials_created: u64,
    /// Number of files in the transfer.
    pub num_files: u64,
    /// Number of regular files.
    pub num_reg_files: u64,
    /// Number of directories.
    pub num_dirs: u64,
    /// Number of symlinks.
    pub num_symlinks: u64,
    /// Number of devices.
    pub num_devices: u64,
    /// Number of special files.
    pub num_specials: u64,
    /// Number of created files.
    pub num_created_files: u64,
    /// Number of deleted files.
    pub num_deleted_files: u64,
    /// Number of regular files transferred.
    pub num_transferred_files: u64,
    /// Total transferred file size (bytes).
    pub total_transferred_size: u64,
    /// Literal data transferred (bytes).
    pub literal_data: u64,
    /// Matched data (bytes).
    pub matched_data: u64,
    /// File list size (bytes).
    pub flist_size: u64,
}

impl TransferStats {
    /// Creates a new empty stats structure.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            total_read: 0,
            total_written: 0,
            total_size: 0,
            flist_buildtime: 0,
            flist_xfertime: 0,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            symlinks_created: 0,
            specials_created: 0,
            num_files: 0,
            num_reg_files: 0,
            num_dirs: 0,
            num_symlinks: 0,
            num_devices: 0,
            num_specials: 0,
            num_created_files: 0,
            num_deleted_files: 0,
            num_transferred_files: 0,
            total_transferred_size: 0,
            literal_data: 0,
            matched_data: 0,
            flist_size: 0,
        }
    }

    /// Creates stats with the given byte counts.
    #[must_use]
    pub const fn with_bytes(total_read: u64, total_written: u64, total_size: u64) -> Self {
        Self {
            total_read,
            total_written,
            total_size,
            flist_buildtime: 0,
            flist_xfertime: 0,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            symlinks_created: 0,
            specials_created: 0,
            num_files: 0,
            num_reg_files: 0,
            num_dirs: 0,
            num_symlinks: 0,
            num_devices: 0,
            num_specials: 0,
            num_created_files: 0,
            num_deleted_files: 0,
            num_transferred_files: 0,
            total_transferred_size: 0,
            literal_data: 0,
            matched_data: 0,
            flist_size: 0,
        }
    }

    /// Sets the file list timing information.
    #[must_use]
    pub const fn with_flist_times(mut self, buildtime: u64, xfertime: u64) -> Self {
        self.flist_buildtime = buildtime;
        self.flist_xfertime = xfertime;
        self
    }

    /// Sets incremental mode statistics.
    #[must_use]
    pub const fn with_incremental_stats(
        mut self,
        entries_received: u64,
        directories_created: u64,
        directories_failed: u64,
        files_skipped: u64,
        symlinks_created: u64,
        specials_created: u64,
    ) -> Self {
        self.entries_received = entries_received;
        self.directories_created = directories_created;
        self.directories_failed = directories_failed;
        self.files_skipped = files_skipped;
        self.symlinks_created = symlinks_created;
        self.specials_created = specials_created;
        self
    }

    /// Writes the stats to a stream in wire format.
    ///
    /// The format depends on the protocol version:
    /// - All versions: total_read, total_written, total_size (varlong30)
    /// - Protocol >= 29: Also includes flist_buildtime, flist_xfertime
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the stream fails.
    pub fn write_to<W: Write>(&self, writer: &mut W, protocol: ProtocolVersion) -> io::Result<()> {
        write_varlong30(writer, self.total_read as i64, 3)?;
        write_varlong30(writer, self.total_written as i64, 3)?;
        write_varlong30(writer, self.total_size as i64, 3)?;

        if protocol.supports_flist_times() {
            write_varlong30(writer, self.flist_buildtime as i64, 3)?;
            write_varlong30(writer, self.flist_xfertime as i64, 3)?;
        }

        Ok(())
    }

    /// Reads stats from a stream in wire format.
    ///
    /// The format depends on the protocol version:
    /// - All versions: total_read, total_written, total_size (varlong30)
    /// - Protocol >= 29: Also includes flist_buildtime, flist_xfertime
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the stream fails.
    pub fn read_from<R: Read>(reader: &mut R, protocol: ProtocolVersion) -> io::Result<Self> {
        let total_read = read_varlong30(reader, 3)? as u64;
        let total_written = read_varlong30(reader, 3)? as u64;
        let total_size = read_varlong30(reader, 3)? as u64;

        let (flist_buildtime, flist_xfertime) = if protocol.supports_flist_times() {
            let buildtime = read_varlong30(reader, 3)? as u64;
            let xfertime = read_varlong30(reader, 3)? as u64;
            (buildtime, xfertime)
        } else {
            (0, 0)
        };

        Ok(Self {
            total_read,
            total_written,
            total_size,
            flist_buildtime,
            flist_xfertime,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            symlinks_created: 0,
            specials_created: 0,
            num_files: 0,
            num_reg_files: 0,
            num_dirs: 0,
            num_symlinks: 0,
            num_devices: 0,
            num_specials: 0,
            num_created_files: 0,
            num_deleted_files: 0,
            num_transferred_files: 0,
            total_transferred_size: 0,
            literal_data: 0,
            matched_data: 0,
            flist_size: 0,
        })
    }

    /// Swaps read/written counts for perspective change.
    ///
    /// When transferring stats between sender and receiver, the meaning
    /// of read/write swaps. This method performs that swap.
    #[must_use]
    pub const fn swap_perspective(self) -> Self {
        Self {
            total_read: self.total_written,
            total_written: self.total_read,
            total_size: self.total_size,
            flist_buildtime: self.flist_buildtime,
            flist_xfertime: self.flist_xfertime,
            entries_received: self.entries_received,
            directories_created: self.directories_created,
            directories_failed: self.directories_failed,
            files_skipped: self.files_skipped,
            symlinks_created: self.symlinks_created,
            specials_created: self.specials_created,
            num_files: self.num_files,
            num_reg_files: self.num_reg_files,
            num_dirs: self.num_dirs,
            num_symlinks: self.num_symlinks,
            num_devices: self.num_devices,
            num_specials: self.num_specials,
            num_created_files: self.num_created_files,
            num_deleted_files: self.num_deleted_files,
            num_transferred_files: self.num_transferred_files,
            total_transferred_size: self.total_transferred_size,
            literal_data: self.literal_data,
            matched_data: self.matched_data,
            flist_size: self.flist_size,
        }
    }
}

impl TransferStats {
    /// Formats a number with comma separators (e.g., 1,234,567).
    fn format_number(n: u64) -> String {
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
        // Number of files line with breakdown
        if self.num_files > 0 {
            write!(f, "Number of files: {}", Self::format_number(self.num_files))?;

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
                parts.push(format!("special: {}", Self::format_number(self.num_specials)));
            }

            if !parts.is_empty() {
                write!(f, " ({})", parts.join(", "))?;
            }
            writeln!(f)?;
        }

        // Number of created files
        if self.num_created_files > 0 {
            writeln!(f, "Number of created files: {}", Self::format_number(self.num_created_files))?;
        }

        // Number of deleted files
        if self.num_deleted_files > 0 {
            writeln!(f, "Number of deleted files: {}", Self::format_number(self.num_deleted_files))?;
        }

        // Number of regular files transferred
        if self.num_transferred_files > 0 {
            writeln!(f, "Number of regular files transferred: {}",
                    Self::format_number(self.num_transferred_files))?;
        }

        // Total file size
        if self.total_size > 0 {
            writeln!(f, "Total file size: {} bytes", Self::format_number(self.total_size))?;
        }

        // Total transferred file size
        if self.total_transferred_size > 0 {
            writeln!(f, "Total transferred file size: {} bytes",
                    Self::format_number(self.total_transferred_size))?;
        }

        // Literal data and matched data - show if we're showing transfer details
        if self.total_transferred_size > 0 || self.literal_data > 0 || self.matched_data > 0 {
            writeln!(f, "Literal data: {} bytes", Self::format_number(self.literal_data))?;
            writeln!(f, "Matched data: {} bytes", Self::format_number(self.matched_data))?;
        }

        // File list size
        if self.flist_size > 0 {
            writeln!(f, "File list size: {}", Self::format_number(self.flist_size))?;
        }

        // File list generation time
        if self.flist_buildtime > 0 {
            let secs = self.flist_buildtime as f64 / 1_000_000.0;
            writeln!(f, "File list generation time: {:.3} seconds", secs)?;
        }

        // File list transfer time
        if self.flist_xfertime > 0 {
            let secs = self.flist_xfertime as f64 / 1_000_000.0;
            writeln!(f, "File list transfer time: {:.3} seconds", secs)?;
        }

        // Total bytes sent and received
        writeln!(f, "Total bytes sent: {}", Self::format_number(self.total_written))?;
        writeln!(f, "Total bytes received: {}", Self::format_number(self.total_read))?;

        // Summary line: "sent X bytes  received Y bytes  Z bytes/sec"
        let bytes_per_sec = self.bytes_per_sec();
        writeln!(f, "sent {} bytes  received {} bytes  {:.2} bytes/sec",
                Self::format_number(self.total_written),
                Self::format_number(self.total_read),
                bytes_per_sec)?;

        // Final speedup line
        let speedup = self.speedup();
        write!(f, "total size is {}  speedup is {:.2}",
              Self::format_number(self.total_size),
              speedup)?;

        Ok(())
    }
}

/// Deletion statistics exchanged via `NDX_DEL_STATS`.
///
/// These are sent separately from transfer stats to report deletion counts
/// when `--delete` is used. Each field counts the number of items of that
/// type that were removed from the destination.
///
/// # Examples
///
/// ```
/// use protocol::DeleteStats;
///
/// let stats = DeleteStats {
///     files: 10,
///     dirs: 3,
///     symlinks: 2,
///     devices: 0,
///     specials: 0,
/// };
/// assert_eq!(stats.total(), 15);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeleteStats {
    /// Number of regular files deleted.
    pub files: u32,
    /// Number of directories deleted.
    pub dirs: u32,
    /// Number of symlinks deleted.
    pub symlinks: u32,
    /// Number of device nodes deleted.
    pub devices: u32,
    /// Number of special files (FIFOs, sockets) deleted.
    pub specials: u32,
}

impl DeleteStats {
    /// Creates a new empty delete stats structure.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files: 0,
            dirs: 0,
            symlinks: 0,
            devices: 0,
            specials: 0,
        }
    }

    /// Returns the total number of items deleted.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.files
            .saturating_add(self.dirs)
            .saturating_add(self.symlinks)
            .saturating_add(self.devices)
            .saturating_add(self.specials)
    }

    /// Writes the delete stats to a stream in wire format.
    ///
    /// Wire format (all varint):
    /// - files (excluding dirs/symlinks/devices/specials)
    /// - dirs
    /// - symlinks
    /// - devices
    /// - specials
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the stream fails.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        use crate::varint::write_varint;

        // Write files count excluding other types (matches upstream)
        write_varint(writer, self.files as i32)?;
        write_varint(writer, self.dirs as i32)?;
        write_varint(writer, self.symlinks as i32)?;
        write_varint(writer, self.devices as i32)?;
        write_varint(writer, self.specials as i32)?;

        Ok(())
    }

    /// Reads delete stats from a stream in wire format.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the stream fails.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        use crate::varint::read_varint;

        // Read in same order as write
        let files = read_varint(reader)? as u32;
        let dirs = read_varint(reader)? as u32;
        let symlinks = read_varint(reader)? as u32;
        let devices = read_varint(reader)? as u32;
        let specials = read_varint(reader)? as u32;

        Ok(Self {
            files,
            dirs,
            symlinks,
            devices,
            specials,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_transfer_stats_roundtrip_proto30() {
        let stats = TransferStats {
            total_read: 1024,
            total_written: 2048,
            total_size: 10000,
            flist_buildtime: 500000,
            flist_xfertime: 100000,
            ..Default::default()
        };

        let protocol = ProtocolVersion::V30;
        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

        // Only wire format fields are preserved
        assert_eq!(decoded.total_read, stats.total_read);
        assert_eq!(decoded.total_written, stats.total_written);
        assert_eq!(decoded.total_size, stats.total_size);
        assert_eq!(decoded.flist_buildtime, stats.flist_buildtime);
        assert_eq!(decoded.flist_xfertime, stats.flist_xfertime);
    }

    #[test]
    fn test_transfer_stats_roundtrip_proto28() {
        let stats = TransferStats {
            total_read: 5000,
            total_written: 3000,
            total_size: 50000,
            ..Default::default()
        };

        let protocol = ProtocolVersion::V28;
        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

        // flist times should be 0 for proto 28
        assert_eq!(decoded.total_read, stats.total_read);
        assert_eq!(decoded.total_written, stats.total_written);
        assert_eq!(decoded.total_size, stats.total_size);
        assert_eq!(decoded.flist_buildtime, 0);
        assert_eq!(decoded.flist_xfertime, 0);
    }

    #[test]
    fn test_transfer_stats_swap_perspective() {
        let stats = TransferStats {
            total_read: 100,
            total_written: 200,
            total_size: 1000,
            flist_buildtime: 50,
            flist_xfertime: 25,
            entries_received: 10,
            directories_created: 5,
            directories_failed: 2,
            files_skipped: 3,
            symlinks_created: 1,
            specials_created: 0,
            num_files: 5,
            num_reg_files: 3,
            ..Default::default()
        };

        let swapped = stats.swap_perspective();

        assert_eq!(swapped.total_read, 200);
        assert_eq!(swapped.total_written, 100);
        assert_eq!(swapped.total_size, 1000);
        assert_eq!(swapped.flist_buildtime, 50);
        assert_eq!(swapped.flist_xfertime, 25);
        assert_eq!(swapped.entries_received, 10);
        assert_eq!(swapped.directories_created, 5);
        assert_eq!(swapped.directories_failed, 2);
        assert_eq!(swapped.files_skipped, 3);
        assert_eq!(swapped.symlinks_created, 1);
        assert_eq!(swapped.specials_created, 0);
        assert_eq!(swapped.num_files, 5);
        assert_eq!(swapped.num_reg_files, 3);
    }

    #[test]
    fn test_transfer_stats_with_builders() {
        let stats = TransferStats::with_bytes(100, 200, 1000).with_flist_times(50000, 25000);

        assert_eq!(stats.total_read, 100);
        assert_eq!(stats.total_written, 200);
        assert_eq!(stats.total_size, 1000);
        assert_eq!(stats.flist_buildtime, 50000);
        assert_eq!(stats.flist_xfertime, 25000);
    }

    #[test]
    fn test_transfer_stats_with_incremental_stats() {
        let stats = TransferStats::new().with_incremental_stats(100, 10, 2, 5, 3, 1);

        assert_eq!(stats.entries_received, 100);
        assert_eq!(stats.directories_created, 10);
        assert_eq!(stats.directories_failed, 2);
        assert_eq!(stats.files_skipped, 5);
        assert_eq!(stats.symlinks_created, 3);
        assert_eq!(stats.specials_created, 1);
    }

    #[test]
    fn test_transfer_stats_display_basic() {
        let stats = TransferStats {
            total_read: 456,
            total_written: 789,
            total_size: 12345,
            num_files: 5,
            num_reg_files: 3,
            num_dirs: 2,
            num_created_files: 2,
            num_deleted_files: 0,
            num_transferred_files: 1,
            total_transferred_size: 4567,
            literal_data: 4567,
            matched_data: 0,
            flist_size: 123,
            flist_buildtime: 1000,    // 0.001 seconds
            flist_xfertime: 0,
            ..Default::default()
        };

        let output = format!("{}", stats);

        // Check that output matches upstream format
        assert!(output.contains("Number of files: 5 (reg: 3, dir: 2)"));
        assert!(output.contains("Number of created files: 2"));
        assert!(output.contains("Number of regular files transferred: 1"));
        assert!(output.contains("Total file size: 12,345 bytes"));
        assert!(output.contains("Total transferred file size: 4,567 bytes"));
        assert!(output.contains("Literal data: 4,567 bytes"));
        assert!(output.contains("Matched data: 0 bytes"));
        assert!(output.contains("File list size: 123"));
        assert!(output.contains("File list generation time: 0.001 seconds"));
        assert!(output.contains("Total bytes sent: 789"));
        assert!(output.contains("Total bytes received: 456"));
        assert!(output.contains("sent 789 bytes  received 456 bytes"));
        assert!(output.contains("total size is 12,345  speedup is"));
    }

    #[test]
    fn test_transfer_stats_display_large_numbers() {
        let stats = TransferStats {
            total_read: 1_234_567,
            total_written: 7_654_321,
            total_size: 123_456_789,
            num_files: 1000,
            num_reg_files: 950,
            num_dirs: 50,
            flist_buildtime: 500000,  // 0.5 seconds
            flist_xfertime: 100000,   // 0.1 seconds
            ..Default::default()
        };

        let output = format!("{}", stats);

        // Verify comma-separated formatting
        assert!(output.contains("Number of files: 1,000 (reg: 950, dir: 50)"));
        assert!(output.contains("Total file size: 123,456,789 bytes"));
        assert!(output.contains("Total bytes sent: 7,654,321"));
        assert!(output.contains("Total bytes received: 1,234,567"));
        assert!(output.contains("sent 7,654,321 bytes  received 1,234,567 bytes"));
        assert!(output.contains("total size is 123,456,789"));
    }

    #[test]
    fn test_transfer_stats_display_with_all_file_types() {
        let stats = TransferStats {
            total_read: 1000,
            total_written: 2000,
            total_size: 50000,
            num_files: 25,
            num_reg_files: 10,
            num_dirs: 5,
            num_symlinks: 7,
            num_devices: 2,
            num_specials: 1,
            flist_buildtime: 100000,
            flist_xfertime: 50000,
            ..Default::default()
        };

        let output = format!("{}", stats);

        // Verify all file types are shown
        assert!(output.contains("Number of files: 25 (reg: 10, dir: 5, link: 7, dev: 2, special: 1)"));
    }

    #[test]
    fn test_transfer_stats_display_speedup_calculation() {
        let stats = TransferStats {
            total_read: 500,
            total_written: 500,
            total_size: 10000,
            flist_buildtime: 1000000,  // 1 second
            flist_xfertime: 1000000,   // 1 second
            ..Default::default()
        };

        let output = format!("{}", stats);

        // Total bytes = 1000, total size = 10000, speedup should be 10.00
        assert!(output.contains("speedup is 10.00"));

        // Total time = 2 seconds, total bytes = 1000, rate = 500 bytes/sec
        assert!(output.contains("500.00 bytes/sec"));
    }

    #[test]
    fn test_transfer_stats_display_minimal() {
        let stats = TransferStats {
            total_read: 100,
            total_written: 200,
            total_size: 0,
            ..Default::default()
        };

        let output = format!("{}", stats);

        // Even with minimal data, should have the summary lines
        assert!(output.contains("Total bytes sent: 200"));
        assert!(output.contains("Total bytes received: 100"));
        assert!(output.contains("sent 200 bytes  received 100 bytes"));
        assert!(output.contains("total size is 0"));
    }

    #[test]
    fn test_transfer_stats_format_number() {
        assert_eq!(TransferStats::format_number(0), "0");
        assert_eq!(TransferStats::format_number(999), "999");
        assert_eq!(TransferStats::format_number(1000), "1,000");
        assert_eq!(TransferStats::format_number(1234), "1,234");
        assert_eq!(TransferStats::format_number(12345), "12,345");
        assert_eq!(TransferStats::format_number(123456), "123,456");
        assert_eq!(TransferStats::format_number(1234567), "1,234,567");
        assert_eq!(TransferStats::format_number(1234567890), "1,234,567,890");
    }

    #[test]
    fn test_transfer_stats_bytes_per_sec_zero_time() {
        let stats = TransferStats {
            total_read: 1000,
            total_written: 2000,
            flist_buildtime: 0,
            flist_xfertime: 0,
            ..Default::default()
        };

        // Should return 0 when time is 0
        assert_eq!(stats.bytes_per_sec(), 0.0);
    }

    #[test]
    fn test_transfer_stats_speedup_zero_bytes() {
        let stats = TransferStats {
            total_read: 0,
            total_written: 0,
            total_size: 1000,
            ..Default::default()
        };

        // Should return 0 when total bytes is 0
        assert_eq!(stats.speedup(), 0.0);
    }

    #[test]
    fn test_transfer_stats_display_matches_upstream_format() {
        // Test that closely matches the example from upstream rsync
        let stats = TransferStats {
            total_read: 456,
            total_written: 789,
            total_size: 12345,
            num_files: 5,
            num_reg_files: 3,
            num_dirs: 2,
            num_created_files: 2,
            num_transferred_files: 1,
            total_transferred_size: 4567,
            literal_data: 4567,
            matched_data: 0,
            flist_size: 123,
            flist_buildtime: 1000,    // 0.001 seconds
            flist_xfertime: 0,        // 0.000 seconds
            ..Default::default()
        };

        let output = format!("{}", stats);
        let lines: Vec<&str> = output.lines().collect();

        // Verify each line matches expected format
        assert_eq!(lines[0], "Number of files: 5 (reg: 3, dir: 2)");
        assert_eq!(lines[1], "Number of created files: 2");
        assert_eq!(lines[2], "Number of regular files transferred: 1");
        assert_eq!(lines[3], "Total file size: 12,345 bytes");
        assert_eq!(lines[4], "Total transferred file size: 4,567 bytes");
        assert_eq!(lines[5], "Literal data: 4,567 bytes");
        assert_eq!(lines[6], "Matched data: 0 bytes");
        assert_eq!(lines[7], "File list size: 123");
        assert_eq!(lines[8], "File list generation time: 0.001 seconds");
        assert_eq!(lines[9], "Total bytes sent: 789");
        assert_eq!(lines[10], "Total bytes received: 456");
        assert!(lines[11].starts_with("sent 789 bytes  received 456 bytes"));
        assert!(lines[12].starts_with("total size is 12,345  speedup is"));
    }

    #[test]
    fn test_transfer_stats_large_values() {
        // Note: varlong30 with min_bytes=3 has a max safe value of ~288 PB.
        // This matches upstream rsync's limitation (io.c only reads 8 bytes max).
        // Use realistic large values that fit within this limit.
        let stats = TransferStats {
            total_read: 100_000_000_000_000,   // 100 TB - realistic large backup
            total_written: 50_000_000_000_000, // 50 TB
            total_size: 200_000_000_000_000,   // 200 TB - large file set
            flist_buildtime: 1_000_000_000,    // 1000 seconds in microseconds
            flist_xfertime: 500_000_000,       // 500 seconds in microseconds
            ..Default::default()
        };

        let protocol = ProtocolVersion::V32;
        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

        // Check wire format fields
        assert_eq!(decoded.total_read, stats.total_read);
        assert_eq!(decoded.total_written, stats.total_written);
        assert_eq!(decoded.total_size, stats.total_size);
        assert_eq!(decoded.flist_buildtime, stats.flist_buildtime);
        assert_eq!(decoded.flist_xfertime, stats.flist_xfertime);
    }

    #[test]
    fn test_delete_stats_roundtrip() {
        let stats = DeleteStats {
            files: 10,
            dirs: 3,
            symlinks: 2,
            devices: 1,
            specials: 0,
        };

        let mut buf = Vec::new();
        stats.write_to(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = DeleteStats::read_from(&mut cursor).unwrap();

        assert_eq!(stats, decoded);
    }

    #[test]
    fn test_delete_stats_total() {
        let stats = DeleteStats {
            files: 10,
            dirs: 5,
            symlinks: 3,
            devices: 2,
            specials: 1,
        };

        assert_eq!(stats.total(), 21);
    }

    #[test]
    fn test_delete_stats_empty() {
        let stats = DeleteStats::new();

        assert_eq!(stats.total(), 0);

        let mut buf = Vec::new();
        stats.write_to(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = DeleteStats::read_from(&mut cursor).unwrap();

        assert_eq!(stats, decoded);
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;

        #[test]
        fn test_transfer_stats_serde_roundtrip() {
            let stats = TransferStats {
                total_read: 1024,
                total_written: 2048,
                total_size: 10000,
                flist_buildtime: 500000,
                flist_xfertime: 100000,
                entries_received: 50,
                directories_created: 10,
                directories_failed: 2,
                files_skipped: 5,
                symlinks_created: 3,
                specials_created: 1,
                num_files: 100,
                num_reg_files: 80,
                num_dirs: 15,
                num_symlinks: 5,
                num_devices: 0,
                num_specials: 0,
                num_created_files: 25,
                num_deleted_files: 5,
                num_transferred_files: 20,
                total_transferred_size: 8000,
                literal_data: 6000,
                matched_data: 2000,
                flist_size: 500,
            };

            let json = serde_json::to_string(&stats).unwrap();
            let decoded: TransferStats = serde_json::from_str(&json).unwrap();
            assert_eq!(stats, decoded);
        }

        #[test]
        fn test_delete_stats_serde_roundtrip() {
            let stats = DeleteStats {
                files: 10,
                dirs: 3,
                symlinks: 2,
                devices: 1,
                specials: 0,
            };

            let json = serde_json::to_string(&stats).unwrap();
            let decoded: DeleteStats = serde_json::from_str(&json).unwrap();
            assert_eq!(stats, decoded);
        }
    }
}
