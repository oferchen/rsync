//! File list reading (decoding) from the rsync wire format.
//!
//! This module implements the receiver side of file list exchange, decoding
//! file entries as they arrive from the sender.

use std::io::{self, Read};
use std::path::PathBuf;

use crate::ProtocolVersion;
use crate::varint::read_varint;

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
    pub fn read_entry<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<Option<FileEntry>> {
        // Read flags (as varint for protocol 30+, as byte for older protocols)
        let flags_value = if self.protocol.as_u8() >= 30 {
            read_varint(reader)?
        } else {
            let mut flags_byte = [0u8; 1];
            reader.read_exact(&mut flags_byte)?;
            flags_byte[0] as i32
        };

        // Zero value marks end of file list
        if flags_value == 0 {
            return Ok(None);
        }

        // Extract flags bytes from varint value
        let flags_byte = (flags_value & 0xFF) as u8;
        let ext_byte = ((flags_value >> 8) & 0xFF) as u8;

        // Build flags structure
        // For protocol 28+, extended flags may be present in the second byte
        let flags = if ext_byte != 0 || (flags_byte & super::flags::XMIT_EXTENDED_FLAGS) != 0 {
            FileFlags::new(flags_byte, ext_byte)
        } else {
            FileFlags::new(flags_byte, 0)
        };

        // Read name with compression
        let name = self.read_name(reader, &flags)?;

        // Read file size
        let size = self.read_size(reader)?;

        // Read mtime (or use previous)
        let mtime = if flags.same_time() {
            self.prev_mtime
        } else {
            // Mtime is written as varlong with min_bytes=4 (upstream flist.c:581-585)
            let mtime = crate::read_varlong(reader, 4)?;
            self.prev_mtime = mtime;
            mtime
        };

        // Read mode (or use previous)
        let mode = if flags.same_mode() {
            self.prev_mode
        } else {
            // Mode is written as 4-byte little-endian i32 (upstream write_int)
            let mut mode_bytes = [0u8; 4];
            reader.read_exact(&mut mode_bytes)?;
            let mode = i32::from_le_bytes(mode_bytes) as u32;
            self.prev_mode = mode;
            mode
        };

        // Construct entry
        let path = PathBuf::from(String::from_utf8_lossy(&name).into_owned());
        let entry = FileEntry::from_raw(path, size, mode, mtime, 0, flags);

        Ok(Some(entry))
    }

    /// Reads the file name with path compression.
    fn read_name<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: &FileFlags,
    ) -> io::Result<Vec<u8>> {
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

    /// Reads the file size using varlong encoding.
    ///
    /// The write side uses write_varlong30(writer, size, 3) which calls write_varlong.
    /// We must use read_varlong with min_bytes=3 to match.
    fn read_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<u64> {
        // In protocol 30+, sizes use varlong with min_bytes=3
        if self.protocol.as_u8() >= 30 {
            // Match write_varlong30(writer, size, 3) from write.rs:133
            let size = crate::read_varlong(reader, 3)?;
            Ok(size as u64)
        } else {
            // Older protocols use 32-bit varint sizes
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
        use super::super::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // Create a simple file entry
        let mut entry = FileEntry::new_file("test".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test");
        assert_eq!(read_entry.size(), 100);
        assert_eq!(read_entry.mode(), 0o100644);
        assert_eq!(read_entry.mtime(), 1700000000);
    }

    #[test]
    fn read_entry_with_name_compression() {
        use super::super::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        // Create two entries with shared prefix to test name compression
        let mut entry1 = FileEntry::new_file("dir/file".into(), 50, 0o100644);
        entry1.set_mtime(1700000000, 0);

        let mut entry2 = FileEntry::new_file("dir/other".into(), 75, 0o100644);
        entry2.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry1).unwrap();
        writer.write_entry(&mut data, &entry2).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        // Read first entry
        let read_entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry1.name(), "dir/file");

        // Read second entry (should use name compression)
        let read_entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry2.name(), "dir/other");
    }
}
