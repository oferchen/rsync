//! Transfer statistics struct and wire format encoding/decoding.
//!
//! Implements the wire format for exchanging transfer statistics between rsync
//! processes. The format varies by protocol version:
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
