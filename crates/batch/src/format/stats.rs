//! Transfer statistics recorded at the end of a batch file.
//!
//! Upstream rsync writes these statistics so the `--read-batch` process
//! can display accurate transfer metrics.

use std::io::{self, Read, Write};

/// Transfer statistics recorded at the end of a batch file.
///
/// # Upstream Reference
///
/// - `main.c:374-383`: `write_varlong30(batch_fd, total_read, 3)` etc.
/// - Stats are written using `varlong30` encoding with 3 minimum bytes.
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
    /// Uses `write_varlong30` with `min_bytes=3` to match upstream rsync's
    /// `main.c` stats serialization.
    pub fn write_to<W: Write>(&self, writer: &mut W, protocol_version: i32) -> io::Result<()> {
        protocol::write_varlong30(writer, self.total_read, 3)?;
        protocol::write_varlong30(writer, self.total_written, 3)?;
        protocol::write_varlong30(writer, self.total_size, 3)?;
        if protocol_version >= 29 {
            protocol::write_varlong30(writer, self.flist_buildtime.unwrap_or(0), 3)?;
            protocol::write_varlong30(writer, self.flist_xfertime.unwrap_or(0), 3)?;
        }
        Ok(())
    }

    /// Read the statistics from a reader.
    ///
    /// Uses `read_varlong30` with `min_bytes=3` to match upstream rsync's
    /// stats deserialization.
    pub fn read_from<R: Read>(reader: &mut R, protocol_version: i32) -> io::Result<Self> {
        let total_read = protocol::read_varlong30(reader, 3)?;
        let total_written = protocol::read_varlong30(reader, 3)?;
        let total_size = protocol::read_varlong30(reader, 3)?;
        let (flist_buildtime, flist_xfertime) = if protocol_version >= 29 {
            let bt = protocol::read_varlong30(reader, 3)?;
            let xt = protocol::read_varlong30(reader, 3)?;
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
