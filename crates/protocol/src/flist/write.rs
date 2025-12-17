//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver.

use std::io::{self, Write};

use crate::ProtocolVersion;
use crate::varint::{write_varint, write_varlong, write_varlong30};

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
    ///
    /// This mirrors upstream rsync's `send_file_entry()` from flist.c.
    /// For protocol 32, flags are written as varint (VARINT_FLIST_FLAGS is set).
    pub fn write_entry<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        let name = entry.name().as_bytes();

        // Calculate name compression (upstream flist.c:532-534)
        let same_len = common_prefix_len(&self.prev_name, name);
        let suffix_len = name.len() - same_len;

        // Build xflags (upstream flist.c:406-540)
        let mut xflags: u32 = 0;

        // File type initialization
        if entry.is_dir() && entry.flags().top_dir() {
            xflags |= XMIT_TOP_DIR as u32;
        }

        // Mode comparison (upstream flist.c:438-440)
        if entry.mode() == self.prev_mode {
            xflags |= XMIT_SAME_MODE as u32;
        } else {
            self.prev_mode = entry.mode();
        }

        // Time comparison (upstream flist.c:494-496)
        if entry.mtime() == self.prev_mtime {
            xflags |= XMIT_SAME_TIME as u32;
        } else {
            self.prev_mtime = entry.mtime();
        }

        // Name compression (upstream flist.c:532-537)
        if same_len > 0 {
            xflags |= XMIT_SAME_NAME as u32;
        }

        if suffix_len > 255 {
            xflags |= XMIT_LONG_NAME as u32;
        }

        // Ensure xflags is non-zero - upstream flist.c:541-547
        // For protocol 30+ with varint encoding, use XMIT_EXTENDED_FLAGS if xflags would be zero
        let xflags_to_write = if xflags == 0 {
            XMIT_EXTENDED_FLAGS as u32
        } else {
            xflags
        };

        // Write xflags as varint for protocol 30+ (upstream flist.c:549-559)
        //
        // VARINT_FLIST_FLAGS compatibility flag (bit 7, 0x80) controls whether flags
        // are encoded as varints or single bytes. Upstream rsync automatically sets
        // this flag for all protocol 30+ sessions during capability negotiation
        // (compat.c:setup_protocol), making it equivalent to a protocol version check.
        //
        // We mirror upstream by checking protocol >= 30 rather than testing the flag,
        // because the flag is always present for these protocols and not checking it
        // matches upstream's actual behavior (they also use protocol version checks
        // in performance-critical paths like flist.c:send_file_entry).
        if self.protocol.as_u8() >= 30 {
            write_varint(writer, xflags_to_write as i32)?;
        } else {
            // Older protocol support (not used for protocol 32, but included for completeness)
            writer.write_all(&[xflags_to_write as u8])?;
        }

        // Write name compression info (upstream flist.c:560-569)
        if xflags & (XMIT_SAME_NAME as u32) != 0 {
            writer.write_all(&[same_len as u8])?;
        }

        // Write suffix length
        if xflags & (XMIT_LONG_NAME as u32) != 0 {
            write_varint(writer, suffix_len as i32)?;
        } else {
            writer.write_all(&[suffix_len as u8])?;
        }

        // Write suffix bytes (upstream flist.c:570)
        writer.write_all(&name[same_len..])?;

        // Write file length using varlong30 (upstream flist.c:580)
        write_varlong30(writer, entry.size() as i64, 3)?;

        // Write mtime if different (upstream flist.c:581-585)
        if xflags & (XMIT_SAME_TIME as u32) == 0 {
            // For protocol >= 30, use write_varlong with min_bytes=4
            write_varlong(writer, entry.mtime(), 4)?;
        }

        // Write mode if different (upstream flist.c:593-594)
        if xflags & (XMIT_SAME_MODE as u32) == 0 {
            // Upstream uses write_int(f, to_wire_mode(mode))
            // to_wire_mode() is usually a no-op on Unix, just converts mode to i32
            let wire_mode = entry.mode() as i32;
            writer.write_all(&wire_mode.to_le_bytes())?;
        }

        // Update previous name (upstream flist.c:677)
        self.prev_name = name.to_vec();

        Ok(())
    }

    /// Writes the end-of-list marker.
    pub fn write_end<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&[0u8])
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
    }
}
