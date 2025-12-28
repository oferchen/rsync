//! File list writing (encoding) to the rsync wire format.
//!
//! This module implements the sender side of file list exchange, encoding
//! file entries for transmission to the receiver.

use std::io::{self, Write};

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::codec::{ProtocolCodec, ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::varint::write_varint;

use super::entry::FileEntry;
use super::flags::{
    XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME, XMIT_SAME_GID, XMIT_SAME_MODE,
    XMIT_SAME_NAME, XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_TOP_DIR,
};
use super::state::FileListCompressionState;

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
    /// Compression state for cross-entry field sharing.
    state: FileListCompressionState,
    /// Whether to preserve (and thus write) UID values to the wire.
    preserve_uid: bool,
    /// Whether to preserve (and thus write) GID values to the wire.
    preserve_gid: bool,
    /// Optional filename encoding converter (for --iconv support).
    iconv: Option<FilenameConverter>,
}

impl FileListWriter {
    /// Creates a new file list writer for the given protocol version.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        Self {
            protocol,
            codec: create_protocol_codec(protocol.as_u8()),
            compat_flags: None,
            state: FileListCompressionState::new(),
            preserve_uid: false,
            preserve_gid: false,
            iconv: None,
        }
    }

    /// Creates a new file list writer with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        Self {
            protocol,
            codec: create_protocol_codec(protocol.as_u8()),
            compat_flags: Some(compat_flags),
            state: FileListCompressionState::new(),
            preserve_uid: false,
            preserve_gid: false,
            iconv: None,
        }
    }

    /// Sets whether UID values should be written to the wire.
    #[must_use]
    pub const fn with_preserve_uid(mut self, preserve: bool) -> Self {
        self.preserve_uid = preserve;
        self
    }

    /// Sets whether GID values should be written to the wire.
    #[must_use]
    pub const fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.preserve_gid = preserve;
        self
    }

    /// Sets the filename encoding converter for iconv support.
    #[must_use]
    pub const fn with_iconv(mut self, converter: FilenameConverter) -> Self {
        self.iconv = Some(converter);
        self
    }

    /// Returns whether varint flag encoding is enabled.
    #[inline]
    fn use_varint_flags(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::VARINT_FLIST_FLAGS))
    }

    /// Returns whether safe file list mode is enabled.
    #[inline]
    fn use_safe_file_list(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::SAFE_FILE_LIST))
            || self.protocol.safe_file_list_always_enabled()
    }

    /// Calculates xflags for an entry based on comparison with previous entry.
    fn calculate_xflags(&self, entry: &FileEntry, same_len: usize, suffix_len: usize) -> u32 {
        let mut xflags: u32 = 0;

        // Directory with top_dir flag
        if entry.is_dir() && entry.flags().top_dir() {
            xflags |= XMIT_TOP_DIR as u32;
        }

        // Mode comparison
        if entry.mode() == self.state.prev_mode {
            xflags |= XMIT_SAME_MODE as u32;
        }

        // Time comparison
        if entry.mtime() == self.state.prev_mtime {
            xflags |= XMIT_SAME_TIME as u32;
        }

        // UID comparison
        let entry_uid = entry.uid().unwrap_or(0);
        if self.preserve_uid && entry_uid == self.state.prev_uid {
            xflags |= XMIT_SAME_UID as u32;
        }

        // GID comparison
        let entry_gid = entry.gid().unwrap_or(0);
        if self.preserve_gid && entry_gid == self.state.prev_gid {
            xflags |= XMIT_SAME_GID as u32;
        }

        // Name compression
        if same_len > 0 {
            xflags |= XMIT_SAME_NAME as u32;
        }

        if suffix_len > 255 {
            xflags |= XMIT_LONG_NAME as u32;
        }

        xflags
    }

    /// Writes flags to the wire in the appropriate format.
    fn write_flags<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        xflags: u32,
        is_dir: bool,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: avoid xflags=0 which looks like end marker
            let flags_to_write = if xflags == 0 {
                XMIT_LONG_NAME as u32
            } else {
                xflags
            };
            write_varint(writer, flags_to_write as i32)?;
        } else if self.protocol.supports_extended_flags() {
            // Protocol 28-29: two-byte encoding if needed
            let mut xflags_to_write = xflags;
            if xflags_to_write == 0 && !is_dir {
                xflags_to_write |= XMIT_TOP_DIR as u32;
            }

            if (xflags_to_write & 0xFF00) != 0 || xflags_to_write == 0 {
                xflags_to_write |= XMIT_EXTENDED_FLAGS as u32;
                writer.write_all(&(xflags_to_write as u16).to_le_bytes())?;
            } else {
                writer.write_all(&[xflags_to_write as u8])?;
            }
        } else {
            // Protocol < 28: single byte
            let flags_to_write = if xflags == 0 && !is_dir {
                XMIT_LONG_NAME as u32
            } else {
                xflags
            };
            writer.write_all(&[flags_to_write as u8])?;
        }
        Ok(())
    }

    /// Writes name compression info and suffix.
    fn write_name<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        name: &[u8],
        same_len: usize,
        suffix_len: usize,
        xflags: u32,
    ) -> io::Result<()> {
        // Write same_len if XMIT_SAME_NAME is set
        if xflags & (XMIT_SAME_NAME as u32) != 0 {
            writer.write_all(&[same_len as u8])?;
        }

        // Write suffix length
        if xflags & (XMIT_LONG_NAME as u32) != 0 {
            self.codec.write_long_name_len(writer, suffix_len)?;
        } else {
            writer.write_all(&[suffix_len as u8])?;
        }

        // Write suffix bytes
        writer.write_all(&name[same_len..])
    }

    /// Writes metadata fields (size, mtime, mode, uid, gid).
    fn write_metadata<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        // Write file size
        self.codec.write_file_size(writer, entry.size() as i64)?;

        // Write mtime if different
        if xflags & (XMIT_SAME_TIME as u32) == 0 {
            self.codec.write_mtime(writer, entry.mtime())?;
        }

        // Write mode if different
        if xflags & (XMIT_SAME_MODE as u32) == 0 {
            let wire_mode = entry.mode() as i32;
            writer.write_all(&wire_mode.to_le_bytes())?;
        }

        // Write UID if preserving and different
        let entry_uid = entry.uid().unwrap_or(0);
        if self.preserve_uid && (xflags & (XMIT_SAME_UID as u32)) == 0 {
            if self.protocol.uses_fixed_encoding() {
                writer.write_all(&(entry_uid as i32).to_le_bytes())?;
            } else {
                write_varint(writer, entry_uid as i32)?;
            }
            self.state.update_uid(entry_uid);
        }

        // Write GID if preserving and different
        let entry_gid = entry.gid().unwrap_or(0);
        if self.preserve_gid && (xflags & (XMIT_SAME_GID as u32)) == 0 {
            if self.protocol.uses_fixed_encoding() {
                writer.write_all(&(entry_gid as i32).to_le_bytes())?;
            } else {
                write_varint(writer, entry_gid as i32)?;
            }
            self.state.update_gid(entry_gid);
        }

        Ok(())
    }

    /// Applies iconv encoding conversion to a filename.
    fn apply_encoding_conversion<'a>(
        &self,
        name: &'a [u8],
    ) -> io::Result<std::borrow::Cow<'a, [u8]>> {
        if let Some(ref converter) = self.iconv {
            match converter.local_to_remote(name) {
                Ok(converted) => Ok(converted),
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("filename encoding conversion failed: {e}"),
                )),
            }
        } else {
            Ok(std::borrow::Cow::Borrowed(name))
        }
    }

    /// Writes a file entry to the stream.
    pub fn write_entry<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        // Step 1: Apply encoding conversion
        let raw_name = entry.name().as_bytes();
        let name = self.apply_encoding_conversion(raw_name)?;

        // Step 2: Calculate name compression
        let same_len = self.state.calculate_name_prefix_len(&name);
        let suffix_len = name.len() - same_len;

        // Step 3: Calculate xflags
        let xflags = self.calculate_xflags(entry, same_len, suffix_len);

        // Step 4: Write flags
        self.write_flags(writer, xflags, entry.is_dir())?;

        // Step 5: Write name
        self.write_name(writer, &name, same_len, suffix_len, xflags)?;

        // Step 6: Write metadata
        self.write_metadata(writer, entry, xflags)?;

        // Step 7: Update state
        self.state.update(
            &name,
            entry.mode(),
            entry.mtime(),
            entry.uid().unwrap_or(0),
            entry.gid().unwrap_or(0),
        );

        Ok(())
    }

    /// Writes the end-of-list marker.
    pub fn write_end<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        io_error: Option<i32>,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: zero flags + error code
            write_varint(writer, 0)?;
            write_varint(writer, io_error.unwrap_or(0))?;
            return Ok(());
        }

        if let Some(error) = io_error
            && self.use_safe_file_list()
        {
            // Error marker + code
            let marker_lo = XMIT_EXTENDED_FLAGS;
            let marker_hi = XMIT_IO_ERROR_ENDLIST;
            writer.write_all(&[marker_lo, marker_hi])?;
            write_varint(writer, error)?;
            return Ok(());
        }

        // Normal end marker
        writer.write_all(&[0u8])
    }
}

/// Writes a single file entry to a writer.
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

        assert!(!buf.is_empty());
        assert_ne!(buf[0], 0);
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

        assert_ne!(buf, vec![0u8]);
        assert!(buf.len() > 1);
        assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
        assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

        use crate::varint::decode_varint;
        let cursor = &buf[2..];
        let (error_code, _) = decode_varint(cursor).unwrap();
        assert_eq!(error_code, 23);
    }

    #[test]
    fn write_end_without_safe_file_list_writes_normal_marker_even_with_error() {
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let writer = FileListWriter::new(protocol);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(23)).unwrap();

        assert_eq!(buf, vec![0u8]);
    }

    #[test]
    fn write_end_with_protocol_31_enables_safe_mode_automatically() {
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let writer = FileListWriter::new(protocol);

        let mut buf = Vec::new();
        writer.write_end(&mut buf, Some(42)).unwrap();

        assert_ne!(buf, vec![0u8]);
        assert!(buf.len() > 1);
        assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
        assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

        use crate::varint::decode_varint;
        let cursor = &buf[2..];
        let (error_code, _) = decode_varint(cursor).unwrap();
        assert_eq!(error_code, 42);
    }

    // Tests for extracted helper methods

    #[test]
    fn calculate_xflags_mode_comparison() {
        let mut writer = FileListWriter::new(test_protocol());
        // FileEntry::new_file includes file type bits (S_IFREG = 0o100000)
        // so mode 0o644 becomes 0o100644
        writer.state.update_mode(0o100644);

        let entry_same = FileEntry::new_file("test".into(), 100, 0o644);
        let entry_diff = FileEntry::new_file("test".into(), 100, 0o755);

        let flags_same = writer.calculate_xflags(&entry_same, 0, 4);
        let flags_diff = writer.calculate_xflags(&entry_diff, 0, 4);

        assert!(flags_same & (XMIT_SAME_MODE as u32) != 0);
        assert!(flags_diff & (XMIT_SAME_MODE as u32) == 0);
    }

    #[test]
    fn calculate_xflags_time_comparison() {
        let mut writer = FileListWriter::new(test_protocol());
        writer.state.update_mtime(1700000000);

        let mut entry_same = FileEntry::new_file("test".into(), 100, 0o644);
        entry_same.set_mtime(1700000000, 0);

        let mut entry_diff = FileEntry::new_file("test".into(), 100, 0o644);
        entry_diff.set_mtime(1700000001, 0);

        let flags_same = writer.calculate_xflags(&entry_same, 0, 4);
        let flags_diff = writer.calculate_xflags(&entry_diff, 0, 4);

        assert!(flags_same & (XMIT_SAME_TIME as u32) != 0);
        assert!(flags_diff & (XMIT_SAME_TIME as u32) == 0);
    }

    #[test]
    fn calculate_xflags_name_compression() {
        let writer = FileListWriter::new(test_protocol());
        let entry = FileEntry::new_file("test".into(), 100, 0o644);

        let flags_no_prefix = writer.calculate_xflags(&entry, 0, 4);
        let flags_with_prefix = writer.calculate_xflags(&entry, 2, 2);
        let flags_long_name = writer.calculate_xflags(&entry, 0, 300);

        assert!(flags_no_prefix & (XMIT_SAME_NAME as u32) == 0);
        assert!(flags_with_prefix & (XMIT_SAME_NAME as u32) != 0);
        assert!(flags_long_name & (XMIT_LONG_NAME as u32) != 0);
    }

    #[test]
    fn use_varint_flags_checks_compat_flags() {
        let protocol = test_protocol();

        let writer_without = FileListWriter::new(protocol);
        assert!(!writer_without.use_varint_flags());

        let writer_with =
            FileListWriter::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
        assert!(writer_with.use_varint_flags());
    }

    #[test]
    fn use_safe_file_list_checks_protocol_and_flags() {
        let writer30 = FileListWriter::new(ProtocolVersion::try_from(30u8).unwrap());
        assert!(!writer30.use_safe_file_list());

        let writer30_safe = FileListWriter::with_compat_flags(
            ProtocolVersion::try_from(30u8).unwrap(),
            CompatibilityFlags::SAFE_FILE_LIST,
        );
        assert!(writer30_safe.use_safe_file_list());

        let writer31 = FileListWriter::new(ProtocolVersion::try_from(31u8).unwrap());
        assert!(writer31.use_safe_file_list());
    }
}
