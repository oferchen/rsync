//! Deletion statistics wire format encoding and decoding.
//!
//! Implements the wire format for `NDX_DEL_STATS` messages sent when `--delete`
//! is used. Each field counts items of a specific type removed from the destination.

use std::io::{self, Read, Write};

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
