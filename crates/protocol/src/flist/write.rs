//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver.

use std::io::{self, Write};

use crate::varint::write_varint;
use crate::ProtocolVersion;

use super::entry::FileEntry;
use super::flags::{
    XMIT_EXTENDED_FLAGS, XMIT_LONG_NAME, XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_TIME,
    XMIT_TOP_DIR,
};

/// State maintained while writing a file list.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This writer maintains the necessary state
/// to encode these compressed entries.
#[derive(Debug)]
pub struct FileListWriter {
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

impl FileListWriter {
    /// Creates a new file list writer for the given protocol version.
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

    /// Writes a file entry to the stream.
    pub fn write_entry<W: Write + ?Sized>(&mut self, writer: &mut W, entry: &FileEntry) -> io::Result<()> {
        let name = entry.name().as_bytes();

        // Calculate name compression
        let same_len = common_prefix_len(&self.prev_name, name);
        let suffix_len = name.len() - same_len;

        // Build flags
        let mut flags: u8 = 0;

        if same_len > 0 {
            flags |= XMIT_SAME_NAME;
        }

        if suffix_len > 255 {
            flags |= XMIT_LONG_NAME;
        }

        if entry.mtime() == self.prev_mtime {
            flags |= XMIT_SAME_TIME;
        }

        if entry.mode() == self.prev_mode {
            flags |= XMIT_SAME_MODE;
        }

        if entry.is_dir() && entry.flags().top_dir() {
            flags |= XMIT_TOP_DIR;
        }

        // Extended flags for protocol 28+
        let need_extended = self.protocol.as_u8() >= 28 && flags == 0;
        if need_extended {
            flags |= XMIT_EXTENDED_FLAGS;
        }

        // Ensure flags byte is non-zero (0 = end of list)
        if flags == 0 {
            // Use any harmless flag to ensure non-zero
            // XMIT_SAME_TIME is safe if we also write the time
            flags = XMIT_EXTENDED_FLAGS;
        }

        // Write flags
        writer.write_all(&[flags])?;

        // Write extended flags if present
        if flags & XMIT_EXTENDED_FLAGS != 0 && self.protocol.as_u8() >= 28 {
            writer.write_all(&[0u8])?; // No extended flags set
        }

        // Write name compression info
        if flags & XMIT_SAME_NAME != 0 {
            writer.write_all(&[same_len as u8])?;
        }

        // Write suffix length
        if flags & XMIT_LONG_NAME != 0 {
            write_varint(writer, suffix_len as i32)?;
        } else {
            writer.write_all(&[suffix_len as u8])?;
        }

        // Write suffix bytes
        writer.write_all(&name[same_len..])?;

        // Write size
        self.write_size(writer, entry.size())?;

        // Write mtime if different
        if flags & XMIT_SAME_TIME == 0 {
            write_varint(writer, entry.mtime() as i32)?;
            self.prev_mtime = entry.mtime();
        }

        // Write mode if different
        if flags & XMIT_SAME_MODE == 0 {
            write_varint(writer, entry.mode() as i32)?;
            self.prev_mode = entry.mode();
        }

        // Update previous name
        self.prev_name = name.to_vec();

        Ok(())
    }

    /// Writes the end-of-list marker.
    pub fn write_end<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&[0u8])
    }

    /// Writes the file size using varint encoding.
    fn write_size<W: Write + ?Sized>(&self, writer: &mut W, size: u64) -> io::Result<()> {
        if self.protocol.as_u8() >= 30 {
            if size > u32::MAX as u64 - 1 {
                // Extended size encoding
                writer.write_all(&[0xFF, 0xFF, 0xFF, 0xFF])?; // Marker for extended
                write_varint(writer, (size >> 32) as i32)?;
                write_varint(writer, size as i32)?;
            } else {
                write_varint(writer, size as i32)?;
            }
        } else {
            write_varint(writer, size as i32)?;
        }
        Ok(())
    }
}

/// Calculates the length of the common prefix between two byte slices.
fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b.iter())
        .take_while(|(x, y)| x == y)
        .count()
        .min(255) // Maximum same_len is 255 (single byte)
}

/// Writes a single file entry to a writer.
///
/// This is a convenience function for writing individual entries without
/// maintaining writer state. For writing multiple entries with compression,
/// use [`FileListWriter`].
pub fn write_file_entry<W: Write>(
    writer: &mut W,
    entry: &FileEntry,
    protocol: ProtocolVersion,
) -> io::Result<()> {
    let mut list_writer = FileListWriter::new(protocol);
    list_writer.write_entry(writer, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_protocol() -> ProtocolVersion {
        ProtocolVersion::try_from(32u8).unwrap()
    }

    #[test]
    fn common_prefix_len_empty() {
        assert_eq!(common_prefix_len(b"", b""), 0);
        assert_eq!(common_prefix_len(b"abc", b""), 0);
        assert_eq!(common_prefix_len(b"", b"abc"), 0);
    }

    #[test]
    fn common_prefix_len_partial() {
        assert_eq!(common_prefix_len(b"abc", b"abd"), 2);
        assert_eq!(common_prefix_len(b"dir/file1", b"dir/file2"), 8);
        assert_eq!(common_prefix_len(b"abc", b"xyz"), 0);
    }

    #[test]
    fn common_prefix_len_full() {
        assert_eq!(common_prefix_len(b"abc", b"abc"), 3);
        assert_eq!(common_prefix_len(b"abc", b"abcdef"), 3);
    }

    #[test]
    fn write_end_marker() {
        let mut buf = Vec::new();
        let writer = FileListWriter::new(test_protocol());
        writer.write_end(&mut buf).unwrap();
        assert_eq!(buf, vec![0u8]);
    }

    #[test]
    fn write_simple_entry() {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(test_protocol());
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);

        writer.write_entry(&mut buf, &entry).unwrap();

        // Should have:
        // - flags byte (non-zero)
        // - extended flags byte (protocol 28+)
        // - suffix length byte
        // - name bytes
        // - size varint
        // - mtime varint
        // - mode varint
        assert!(!buf.is_empty());
        assert_ne!(buf[0], 0); // Non-zero flags
    }

    #[test]
    fn write_multiple_entries_with_compression() {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(test_protocol());

        let entry1 = FileEntry::new_file("dir/file1.txt".into(), 100, 0o644);
        let entry2 = FileEntry::new_file("dir/file2.txt".into(), 200, 0o644);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();

        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;

        // Second entry should be shorter due to name compression
        // (shares "dir/file" prefix)
        assert!(second_len < first_len, "second entry should be compressed");
    }

    #[test]
    fn write_then_read_round_trip() {
        use super::super::read::FileListReader;
        use std::io::Cursor;

        let protocol = test_protocol();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf).unwrap();

        // Now read it back
        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.size(), 1024);
        // Mode and mtime depend on flags encoding
    }
}
