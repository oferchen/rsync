//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver.

use std::io::{self, Write};

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::codec::{ProtocolCodec, ProtocolCodecEnum, create_protocol_codec};
use crate::varint::write_varint;

use super::entry::FileEntry;
use super::flags::{
    XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_SAME_MODE, XMIT_SAME_NAME,
    XMIT_SAME_TIME, XMIT_TOP_DIR,
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
    /// Protocol codec for version-aware encoding.
    codec: ProtocolCodecEnum,
    /// Compatibility flags for this session.
    compat_flags: Option<CompatibilityFlags>,
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
            codec: create_protocol_codec(protocol.as_u8()),
            compat_flags: None,
            prev_name: Vec::new(),
            prev_mode: 0,
            prev_mtime: 0,
            prev_uid: 0,
            prev_gid: 0,
        }
    }

    /// Creates a new file list writer with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        Self {
            protocol,
            codec: create_protocol_codec(protocol.as_u8()),
            compat_flags: Some(compat_flags),
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
        // Write entry bytes directly to the writer
        self.write_entry_to_buffer(writer, entry)
    }

    /// Internal: write entry bytes to a writer.
    fn write_entry_to_buffer<W: Write + ?Sized>(
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

        // Check if varint encoding is enabled via VARINT_FLIST_FLAGS compat flag
        // IMPORTANT: Use compat flag, NOT protocol version alone!
        //
        // The server only sets VARINT_FLIST_FLAGS if the client advertises 'v' capability.
        // A client could connect with protocol 30+ but WITHOUT 'v', in which case
        // single-byte flags must be used. This is critical for daemon client interop.
        //
        // Upstream flist.c:send_file_entry() uses xfer_flags_as_varint which is set
        // based on the negotiated compat flags (compat.c:775).
        let use_varint_flags = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));

        // Write xflags (upstream flist.c:549-559)
        if use_varint_flags {
            // Protocol 30+ with varint: use XMIT_EXTENDED_FLAGS if xflags would be zero
            let xflags_to_write = if xflags == 0 {
                XMIT_EXTENDED_FLAGS as u32
            } else {
                xflags
            };
            write_varint(writer, xflags_to_write as i32)?;
        } else if self.protocol.as_u8() >= 28 {
            // Protocol 28-29: upstream flist.c:551-558
            // If xflags is 0 and not a directory, add XMIT_TOP_DIR
            let mut xflags_to_write = xflags;
            if xflags_to_write == 0 && !entry.is_dir() {
                xflags_to_write |= XMIT_TOP_DIR as u32;
            }

            // If high byte is set OR xflags is still 0, use 2-byte encoding
            if (xflags_to_write & 0xFF00) != 0 || xflags_to_write == 0 {
                xflags_to_write |= XMIT_EXTENDED_FLAGS as u32;
                // write_shortint: 2 bytes little-endian
                writer.write_all(&(xflags_to_write as u16).to_le_bytes())?;
            } else {
                // 1 byte encoding
                writer.write_all(&[xflags_to_write as u8])?;
            }
        } else {
            // Protocol < 28: simple byte encoding
            let xflags_to_write = if xflags == 0 {
                XMIT_EXTENDED_FLAGS as u32
            } else {
                xflags
            };
            writer.write_all(&[xflags_to_write as u8])?;
        }

        // Write name compression info (upstream flist.c:560-569)
        if xflags & (XMIT_SAME_NAME as u32) != 0 {
            writer.write_all(&[same_len as u8])?;
        }

        // Write suffix length (upstream flist.c:566-569 -> io.h:write_varint30)
        // Uses codec for protocol-aware encoding:
        // - Protocol >= 30: varint (variable length)
        // - Protocol < 30: 4-byte fixed integer
        // Without XMIT_LONG_NAME: write_byte (1 byte)
        if xflags & (XMIT_LONG_NAME as u32) != 0 {
            self.codec.write_long_name_len(writer, suffix_len)?;
        } else {
            writer.write_all(&[suffix_len as u8])?;
        }

        // Write suffix bytes (upstream flist.c:570)
        writer.write_all(&name[same_len..])?;

        // Write file length (upstream flist.c:580 -> io.h:write_varlong30)
        // Uses codec for protocol-aware encoding:
        // - Protocol >= 30: varlong with min_bytes=3
        // - Protocol < 30: longint (4 bytes for small, 12 for large)
        self.codec.write_file_size(writer, entry.size() as i64)?;

        // Write mtime if different (upstream flist.c:581-585)
        // Uses codec for protocol-aware encoding:
        // - Protocol >= 30: varlong with min_bytes=4
        // - Protocol < 30: 4-byte fixed integer
        if xflags & (XMIT_SAME_TIME as u32) == 0 {
            self.codec.write_mtime(writer, entry.mtime())?;
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
    ///
    /// When `io_error` is provided and the `SAFE_FILE_LIST` flag is enabled,
    /// writes an error marker followed by the error code (mirroring upstream
    /// rsync's `write_end_of_flist(f, send_io_error_list)` from flist.c).
    /// Otherwise writes a simple zero byte marker.
    ///
    /// The SAFE_FILE_LIST flag is automatically enabled for protocol 31+ or
    /// when explicitly negotiated via compat flags (upstream compat.c:775).
    pub fn write_end<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        io_error: Option<i32>,
    ) -> io::Result<()> {
        // Check if varint flist flags mode is enabled (xfer_flags_as_varint in upstream)
        // This is set when VARINT_FLIST_FLAGS compat flag is negotiated
        let xfer_flags_as_varint = if let Some(flags) = self.compat_flags {
            flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
        } else {
            false
        };

        // Check if safe file list mode is enabled (compat.c:775):
        // use_safe_inc_flist = (compat_flags & CF_SAFE_FLIST) || protocol_version >= 31
        let use_safe_inc_flist = if let Some(flags) = self.compat_flags {
            flags.contains(CompatibilityFlags::SAFE_FILE_LIST)
        } else {
            false
        } || self.protocol.as_u8() >= 31;

        // Upstream write_end_of_flist() logic (flist.c):
        // if (xfer_flags_as_varint) {
        //     write_varint(f, 0);
        //     write_varint(f, send_io_error ? io_error : 0);
        // } else if (send_io_error) {
        //     write_shortint(f, XMIT_EXTENDED_FLAGS|XMIT_IO_ERROR_ENDLIST);
        //     write_varint(f, io_error);
        // } else
        //     write_byte(f, 0);

        if xfer_flags_as_varint {
            // Protocol 30+ with VARINT_FLIST_FLAGS: always write two varints
            write_varint(writer, 0)?; // End marker
            write_varint(writer, io_error.unwrap_or(0))?; // Error code (0 = success)
            return Ok(());
        }

        if let Some(error) = io_error
            && use_safe_inc_flist
        {
            // Send error marker with code (upstream flist.c:send_end_of_flist)
            // Uses write_shortint for the marker (2 bytes little-endian), then varint for error
            // write_shortint(f, XMIT_EXTENDED_FLAGS|XMIT_IO_ERROR_ENDLIST);
            // write_varint(f, io_error);
            let marker_lo = XMIT_EXTENDED_FLAGS;
            let marker_hi = XMIT_IO_ERROR_ENDLIST;
            writer.write_all(&[marker_lo, marker_hi])?;
            write_varint(writer, error)?;
            return Ok(());
        }

        // Normal end of list marker (legacy mode)
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
        writer.write_end(&mut buf, None).unwrap();
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
        writer.write_end(&mut buf, None).unwrap();

        // Now read it back
        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test.txt");
        assert_eq!(read_entry.size(), 1024);
    }

    #[test]
    fn write_end_with_safe_file_list_enabled_transmits_error() {
        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST;
        let writer = FileListWriter::with_compat_flags(protocol, flags);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(23)).unwrap();

        // Should have written error marker + error code, not simple 0 byte
        assert_ne!(buf, vec![0u8], "should not write simple end marker");
        assert!(buf.len() > 1, "should have error marker and error code");

        // Verify error marker format (non-varint mode):
        // First byte: XMIT_EXTENDED_FLAGS (0x04)
        // Second byte: XMIT_IO_ERROR_ENDLIST (0x10)
        // Then varint error code
        assert_eq!(
            buf[0], XMIT_EXTENDED_FLAGS,
            "first byte should be XMIT_EXTENDED_FLAGS"
        );
        assert_eq!(
            buf[1], XMIT_IO_ERROR_ENDLIST,
            "second byte should be XMIT_IO_ERROR_ENDLIST"
        );

        // Third varint should be the error code
        use crate::varint::decode_varint;
        let cursor = &buf[2..];
        let (error_code, _) = decode_varint(cursor).unwrap();
        assert_eq!(error_code, 23);
    }

    #[test]
    fn write_end_without_safe_file_list_writes_normal_marker_even_with_error() {
        // Use protocol 30 to avoid automatic safe mode (protocol >= 31)
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let writer = FileListWriter::new(protocol);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(23)).unwrap();

        // Without SAFE_FILE_LIST, should write normal end marker even with error
        assert_eq!(buf, vec![0u8]);
    }

    #[test]
    fn write_end_with_protocol_31_enables_safe_mode_automatically() {
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let writer = FileListWriter::new(protocol); // No explicit compat flags

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(42)).unwrap();

        // Protocol 31+ automatically enables safe mode (uses 2-byte marker format)
        assert_ne!(buf, vec![0u8]);
        assert!(buf.len() > 1);

        // Verify marker format and error code
        assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
        assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

        use crate::varint::decode_varint;
        let cursor = &buf[2..]; // Skip 2-byte marker
        let (error_code, _) = decode_varint(cursor).unwrap();
        assert_eq!(error_code, 42);
    }
}
