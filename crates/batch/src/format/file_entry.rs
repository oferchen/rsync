//! File metadata for batch mode tracking.
//!
//! Note that upstream rsync's batch file format does **not** use a custom
//! file-entry serialization - the batch file body is a raw tee of the
//! protocol stream (flist + delta bytes). This type is provided for
//! internal tracking purposes only.

use std::io::{self, Read, Write};

use super::wire::{read_i32, read_u32, read_u64, read_varint, write_i32, write_string, write_u32, write_u64, write_varint};

/// File metadata for batch mode tracking.
///
/// Holds metadata about a single file/directory/link for batch mode
/// bookkeeping. The `write_to` and `read_from` methods use a local
/// encoding that is not compatible with upstream rsync's batch files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path from source root.
    pub path: String,
    /// File mode bits (permissions + type).
    pub mode: u32,
    /// File size in bytes.
    pub size: u64,
    /// Modification time (seconds since Unix epoch).
    pub mtime: i64,
    /// Owner user ID (if preserved).
    pub uid: Option<u32>,
    /// Owner group ID (if preserved).
    pub gid: Option<u32>,
}

impl FileEntry {
    /// Create a new file entry.
    pub const fn new(path: String, mode: u32, size: u64, mtime: i64) -> Self {
        Self {
            path,
            mode,
            size,
            mtime,
            uid: None,
            gid: None,
        }
    }

    /// Write the file entry to a writer.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        write_string(writer, &self.path)?;
        write_u32(writer, self.mode)?;
        write_u64(writer, self.size)?;
        write_i32(writer, self.mtime as i32)?;

        if let Some(uid) = self.uid {
            write_varint(writer, 1)?;
            write_u32(writer, uid)?;
        } else {
            write_varint(writer, 0)?;
        }

        if let Some(gid) = self.gid {
            write_varint(writer, 1)?;
            write_u32(writer, gid)?;
        } else {
            write_varint(writer, 0)?;
        }

        Ok(())
    }

    /// Read a file entry from a reader.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let path = super::wire::read_string(reader)?;
        let mode = read_u32(reader)?;
        let size = read_u64(reader)?;
        let mtime = read_i32(reader)? as i64;

        let uid = if read_varint(reader)? != 0 {
            Some(read_u32(reader)?)
        } else {
            None
        };

        let gid = if read_varint(reader)? != 0 {
            Some(read_u32(reader)?)
        } else {
            None
        };

        Ok(Self {
            path,
            mode,
            size,
            mtime,
            uid,
            gid,
        })
    }
}
