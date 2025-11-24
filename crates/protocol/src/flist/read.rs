//! File list reading (decoding) from the rsync wire format.
//!
//! This module implements the receiver side of file list exchange, decoding
//! file entries as they arrive from the sender.

use std::io::{self, Read};
use std::path::PathBuf;

use crate::varint::read_varint;
use crate::ProtocolVersion;

use super::entry::FileEntry;
use super::flags::FileFlags;

/// State maintained while reading a file list.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This reader maintains the necessary state
/// to decode these compressed entries.
#[derive(Debug)]
pub struct FileListReader {
    /// Protocol version being used.
    protocol: ProtocolVersion,
    /// Previous entry's path (for name compression).
    prev_name: Vec<u8>,
    /// Previous entry's mode.
    prev_mode: u32,
    /// Previous entry's mtime.
    prev_mtime: i64,
    /// Previous entry's UID (for future ownership preservation).
    #[allow(dead_code)]
    prev_uid: u32,
    /// Previous entry's GID (for future ownership preservation).
    #[allow(dead_code)]
    prev_gid: u32,
}

impl FileListReader {
    /// Creates a new file list reader for the given protocol version.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        Self {
            protocol,
            prev_name: Vec::new(),
            prev_mode: 0,
            prev_mtime: 0,
            prev_uid: 0,
            prev_gid: 0,
        }
    }

    /// Reads the next file entry from the stream.
    ///
    /// Returns `None` when the end-of-list marker is received (a zero byte).
    /// Returns an error on I/O failure or malformed data.
    pub fn read_entry<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<Option<FileEntry>> {
        // Read flags byte
        let mut flags_byte = [0u8; 1];
        reader.read_exact(&mut flags_byte)?;

        // Zero byte marks end of file list
        if flags_byte[0] == 0 {
            return Ok(None);
        }

        let mut flags = FileFlags::new(flags_byte[0], 0);

        // Read extended flags if present (protocol 28+)
        if flags.has_extended() && self.protocol.as_u8() >= 28 {
            let mut ext_byte = [0u8; 1];
            reader.read_exact(&mut ext_byte)?;
            flags = FileFlags::new(flags_byte[0], ext_byte[0]);
        }

        // Read name with compression
        let name = self.read_name(reader, &flags)?;

        // Read file size
        let size = self.read_size(reader)?;

        // Read mtime (or use previous)
        let mtime = if flags.same_time() {
            self.prev_mtime
        } else {
            let mtime = read_varint(reader)? as i64;
            self.prev_mtime = mtime;
            mtime
        };

        // Read mode (or use previous)
        let mode = if flags.same_mode() {
            self.prev_mode
        } else {
            let mode = read_varint(reader)? as u32;
            self.prev_mode = mode;
            mode
        };

        // Construct entry
        let path = PathBuf::from(String::from_utf8_lossy(&name).into_owned());
        let entry = FileEntry::from_raw(path, size, mode, mtime, 0, flags);

        Ok(Some(entry))
    }

    /// Reads the file name with path compression.
    fn read_name<R: Read + ?Sized>(&mut self, reader: &mut R, flags: &FileFlags) -> io::Result<Vec<u8>> {
        // Determine how many bytes are shared with the previous name
        let same_len = if flags.same_name() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        } else {
            0
        };

        // Read the suffix length
        let suffix_len = if flags.long_name() {
            read_varint(reader)? as usize
        } else {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        };

        // Validate lengths
        if same_len > self.prev_name.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "same_len {} exceeds previous name length {}",
                    same_len,
                    self.prev_name.len()
                ),
            ));
        }

        // Build the full name
        let mut name = Vec::with_capacity(same_len + suffix_len);
        name.extend_from_slice(&self.prev_name[..same_len]);

        // Read the suffix bytes
        if suffix_len > 0 {
            let start = name.len();
            name.resize(start + suffix_len, 0);
            reader.read_exact(&mut name[start..])?;
        }

        // Update previous name for next entry
        self.prev_name = name.clone();

        Ok(name)
    }

    /// Reads the file size using varint encoding.
    fn read_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<u64> {
        // In protocol 30+, large files use a two-part encoding
        if self.protocol.as_u8() >= 30 {
            let low = read_varint(reader)? as u32;
            if low == u32::MAX {
                // Extended size follows
                let high = read_varint(reader)? as u64;
                let low2 = read_varint(reader)? as u64;
                Ok((high << 32) | low2)
            } else {
                Ok(low as u64)
            }
        } else {
            // Older protocols use 32-bit sizes
            Ok(read_varint(reader)? as u64)
        }
    }
}

/// Reads a single file entry from a reader.
///
/// This is a convenience function for reading individual entries without
/// maintaining reader state. For reading multiple entries, use [`FileListReader`].
pub fn read_file_entry<R: Read>(
    reader: &mut R,
    protocol: ProtocolVersion,
) -> io::Result<Option<FileEntry>> {
    let mut list_reader = FileListReader::new(protocol);
    list_reader.read_entry(reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::flags::{XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_TIME};
    use std::io::Cursor;

    fn test_protocol() -> ProtocolVersion {
        ProtocolVersion::try_from(32u8).unwrap()
    }

    #[test]
    fn read_end_of_list_marker() {
        let data = [0u8]; // End of list marker
        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(test_protocol());

        let result = reader.read_entry(&mut cursor).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_simple_entry() {
        // Construct a simple file entry:
        // Use XMIT_SAME_TIME | XMIT_SAME_MODE = 0x60
        // No XMIT_SAME_NAME means we don't read same_len byte
        let flags = XMIT_SAME_TIME | XMIT_SAME_MODE;

        let mut data = Vec::new();
        data.push(flags); // flags (0x60)
        // No same_len byte because XMIT_SAME_NAME is not set
        data.push(4); // suffix_len = 4
        data.extend_from_slice(b"test"); // name
        data.push(100); // size (varint, small value)

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(test_protocol());
        // Set previous mode/mtime so SAME_* flags work
        reader.prev_mode = 0o100644;
        reader.prev_mtime = 1700000000;

        let entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(entry.name(), "test");
        assert_eq!(entry.size(), 100);
        assert_eq!(entry.mode(), 0o100644);
        assert_eq!(entry.mtime(), 1700000000);
    }

    #[test]
    fn read_entry_with_name_compression() {
        // First entry (no name compression)
        let flags1 = XMIT_SAME_TIME | XMIT_SAME_MODE;
        let mut data = Vec::new();
        data.push(flags1);
        // No same_len byte because XMIT_SAME_NAME is not set
        data.push(8); // suffix_len = 8
        data.extend_from_slice(b"dir/file"); // name
        data.push(50); // size

        // Second entry with shared prefix (uses XMIT_SAME_NAME)
        let flags2 = XMIT_SAME_NAME | XMIT_SAME_TIME | XMIT_SAME_MODE;
        data.push(flags2);
        data.push(4); // same_len = 4 (shares "dir/")
        data.push(5); // suffix_len = 5
        data.extend_from_slice(b"other"); // suffix
        data.push(75); // size

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(test_protocol());
        reader.prev_mode = 0o100644;
        reader.prev_mtime = 1700000000;

        // Read first entry
        let entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(entry1.name(), "dir/file");

        // Read second entry with compression
        let entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(entry2.name(), "dir/other");
    }
}
