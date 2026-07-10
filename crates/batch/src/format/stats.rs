//! Transfer statistics recorded at the end of a batch file.
//!
//! Upstream rsync writes these statistics so the `--read-batch` process
//! can display accurate transfer metrics.

use std::io::{self, Read, Write};

/// Writes one stats field using upstream's `write_varlong30` macro semantics.
///
/// upstream: main.c:374 writes the batch stats via `write_varlong30(batch_fd,
/// x, 3)`, and io.h:46 `write_varlong30()` gates on the protocol version.
/// Protocol below 30 falls back to `write_longint()` (fixed 4-byte
/// little-endian, widening to 12 bytes for values that exceed a signed 32-bit
/// int); protocol 30 and above uses the variable-length `write_varlong()` with
/// `min_bytes`. A pre-30 `--read-batch` peer decodes these with
/// `read_longint()`, so emitting the varlong form for such a batch file
/// corrupts the trailing stats section. Mirrors
/// `protocol::stats::transfer::write_stat`.
fn write_stat<W: Write>(
    writer: &mut W,
    value: i64,
    min_bytes: u8,
    protocol_version: i32,
) -> io::Result<()> {
    if protocol_version < 30 {
        protocol::write_longint(writer, value)
    } else {
        protocol::write_varlong(writer, value, min_bytes)
    }
}

/// Reads one stats field using upstream's `read_varlong30` macro semantics.
///
/// upstream: io.h:29 `read_varlong30()` - the read-side counterpart to
/// [`write_stat`]; protocol < 30 uses `read_longint()`, protocol >= 30 uses
/// `read_varlong()` with `min_bytes`.
fn read_stat<R: Read>(reader: &mut R, min_bytes: u8, protocol_version: i32) -> io::Result<i64> {
    if protocol_version < 30 {
        protocol::read_longint(reader)
    } else {
        protocol::read_varlong(reader, min_bytes)
    }
}

/// Transfer statistics recorded at the end of a batch file.
///
/// # Upstream Reference
///
/// - `main.c:374-383`: `write_varlong30(batch_fd, total_read, 3)` etc.
/// - Stats use `varlong30` encoding: fixed 4-byte longint for protocol below
///   30, variable-length varlong (`min_bytes=3`) for protocol 30 and above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchStats {
    /// Total bytes read from the source during transfer.
    pub total_read: i64,
    /// Total bytes written to the destination during transfer.
    pub total_written: i64,
    /// Sum of all file sizes in the file list.
    pub total_size: i64,
    /// Time spent building the file list (protocol >= 29).
    pub flist_buildtime: Option<i64>,
    /// Time spent transferring the file list (protocol >= 29).
    pub flist_xfertime: Option<i64>,
}

impl BatchStats {
    /// Write the statistics to a writer.
    ///
    /// Encodes each field with `varlong30` semantics gated on
    /// `protocol_version` (fixed longint for < 30, varlong `min_bytes=3` for
    /// >= 30) to match upstream rsync's `main.c` stats serialization.
    pub fn write_to<W: Write>(&self, writer: &mut W, protocol_version: i32) -> io::Result<()> {
        write_stat(writer, self.total_read, 3, protocol_version)?;
        write_stat(writer, self.total_written, 3, protocol_version)?;
        write_stat(writer, self.total_size, 3, protocol_version)?;
        if protocol_version >= 29 {
            write_stat(
                writer,
                self.flist_buildtime.unwrap_or(0),
                3,
                protocol_version,
            )?;
            write_stat(
                writer,
                self.flist_xfertime.unwrap_or(0),
                3,
                protocol_version,
            )?;
        }
        Ok(())
    }

    /// Read the statistics from a reader.
    ///
    /// Decodes each field with `varlong30` semantics gated on
    /// `protocol_version` to match upstream rsync's stats deserialization.
    pub fn read_from<R: Read>(reader: &mut R, protocol_version: i32) -> io::Result<Self> {
        let total_read = read_stat(reader, 3, protocol_version)?;
        let total_written = read_stat(reader, 3, protocol_version)?;
        let total_size = read_stat(reader, 3, protocol_version)?;
        let (flist_buildtime, flist_xfertime) = if protocol_version >= 29 {
            let bt = read_stat(reader, 3, protocol_version)?;
            let xt = read_stat(reader, 3, protocol_version)?;
            (Some(bt), Some(xt))
        } else {
            (None, None)
        };
        Ok(Self {
            total_read,
            total_written,
            total_size,
            flist_buildtime,
            flist_xfertime,
        })
    }
}
