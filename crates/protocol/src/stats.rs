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
        }
    }

    /// Sets the file list timing information.
    #[must_use]
    pub const fn with_flist_times(mut self, buildtime: u64, xfertime: u64) -> Self {
        self.flist_buildtime = buildtime;
        self.flist_xfertime = xfertime;
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

        if protocol.as_u8() >= 29 {
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

        let (flist_buildtime, flist_xfertime) = if protocol.as_u8() >= 29 {
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
        }
    }
}

/// Deletion statistics exchanged via NDX_DEL_STATS.
///
/// These are sent separately from transfer stats to report deletion counts
/// when `--delete` is used.
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
        };

        let protocol = ProtocolVersion::V30;
        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

        assert_eq!(stats, decoded);
    }

    #[test]
    fn test_transfer_stats_roundtrip_proto28() {
        let stats = TransferStats {
            total_read: 5000,
            total_written: 3000,
            total_size: 50000,
            flist_buildtime: 0, // Not included in proto < 29
            flist_xfertime: 0,
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
        };

        let swapped = stats.swap_perspective();

        assert_eq!(swapped.total_read, 200);
        assert_eq!(swapped.total_written, 100);
        assert_eq!(swapped.total_size, 1000);
        assert_eq!(swapped.flist_buildtime, 50);
        assert_eq!(swapped.flist_xfertime, 25);
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
    fn test_transfer_stats_with_builders() {
        let stats = TransferStats::with_bytes(100, 200, 1000).with_flist_times(50000, 25000);

        assert_eq!(stats.total_read, 100);
        assert_eq!(stats.total_written, 200);
        assert_eq!(stats.total_size, 1000);
        assert_eq!(stats.flist_buildtime, 50000);
        assert_eq!(stats.flist_xfertime, 25000);
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
        };

        let protocol = ProtocolVersion::V32;
        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

        assert_eq!(stats, decoded);
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
