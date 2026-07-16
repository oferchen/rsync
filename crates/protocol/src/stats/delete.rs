//! Deletion statistics wire format encoding and decoding.
//!
//! Implements the wire format for `NDX_DEL_STATS` messages sent when `--delete`
//! is used. Each field counts items of a specific type removed from the destination.

use std::io::{self, Read, Write};

/// Maximum acceptable value for a single delete-stats wire field.
///
/// Rejects unreasonably large values from the wire to prevent signed-integer
/// overflow when the receiver accumulates counts across multiple
/// `NDX_DEL_STATS` messages.
///
/// upstream: io.c - MAX_WIRE_DEL_STAT defence-in-depth (3.4.3)
const MAX_WIRE_DEL_STAT: i32 = 0x3FFF_FFFF;

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
    /// Encodes the five counters (files, dirs, symlinks, devices, specials)
    /// in order, each as a varint.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the stream fails.
    pub fn write_to<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
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
    /// Each field is capped at `MAX_WIRE_DEL_STAT` to reject unreasonably
    /// large values from the wire before they can cause overflow during
    /// accumulation.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the stream fails or if any field
    /// exceeds `MAX_WIRE_DEL_STAT`.
    pub fn read_from<R: Read + ?Sized>(reader: &mut R) -> io::Result<Self> {
        use crate::varint::read_varint;

        let files = read_capped_del_stat(read_varint(reader)?, "files")?;
        let dirs = read_capped_del_stat(read_varint(reader)?, "dirs")?;
        let symlinks = read_capped_del_stat(read_varint(reader)?, "symlinks")?;
        let devices = read_capped_del_stat(read_varint(reader)?, "devices")?;
        let specials = read_capped_del_stat(read_varint(reader)?, "specials")?;

        Ok(Self {
            files,
            dirs,
            symlinks,
            devices,
            specials,
        })
    }

    /// Async twin of [`read_from`](Self::read_from).
    ///
    /// Reads the same five varints (`.await`-driven) in the same order and
    /// applies the identical `read_capped_del_stat` validation, so it yields
    /// the same `DeleteStats` and consumes the same bytes for the same wire
    /// input. Gated on `tokio-transfer`.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the stream fails or if any field
    /// exceeds `MAX_WIRE_DEL_STAT`.
    #[cfg(feature = "tokio-transfer")]
    pub async fn read_from_async<R>(reader: &mut R) -> io::Result<Self>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        use crate::varint::read_varint_async;

        let files = read_capped_del_stat(read_varint_async(reader).await?, "files")?;
        let dirs = read_capped_del_stat(read_varint_async(reader).await?, "dirs")?;
        let symlinks = read_capped_del_stat(read_varint_async(reader).await?, "symlinks")?;
        let devices = read_capped_del_stat(read_varint_async(reader).await?, "devices")?;
        let specials = read_capped_del_stat(read_varint_async(reader).await?, "specials")?;

        Ok(Self {
            files,
            dirs,
            symlinks,
            devices,
            specials,
        })
    }
}

/// Validates and converts a varint-decoded delete-stat field.
///
/// Rejects negative values and values exceeding [`MAX_WIRE_DEL_STAT`].
///
/// upstream: io.c - MAX_WIRE_DEL_STAT defence-in-depth (3.4.3)
fn read_capped_del_stat(raw: i32, field: &str) -> io::Result<u32> {
    if !(0..=MAX_WIRE_DEL_STAT).contains(&raw) {
        // upstream: main.c:read_del_stats() reads each field via
        // read_varint_bounded(f, 0, MAX_WIRE_DEL_STAT, ...), which aborts with
        // exit_cleanup(RERR_PROTOCOL) (exit 2) on an out-of-range value. Tag the
        // error so the core exit-code mapper yields RERR_PROTOCOL, not the
        // generic RERR_STREAMIO (12) that a bare InvalidData maps to.
        return Err(crate::protocol_violation::protocol_violation(format!(
            "delete stat '{field}' value {raw} exceeds MAX_WIRE_DEL_STAT ({MAX_WIRE_DEL_STAT})"
        )));
    }
    Ok(raw as u32)
}
