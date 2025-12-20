//! File list reading (decoding) from the rsync wire format.
//!
//! This module implements the receiver side of file list exchange, decoding
//! file entries as they arrive from the sender.

use std::io::{self, Read};
use std::path::PathBuf;

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::varint::read_varint;

use super::entry::FileEntry;
use super::flags::{FileFlags, XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST};

/// State maintained while reading a file list.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This reader maintains the necessary state
/// to decode these compressed entries.
#[derive(Debug)]
pub struct FileListReader {
    /// Protocol version being used.
    protocol: ProtocolVersion,
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

impl FileListReader {
    /// Creates a new file list reader for the given protocol version.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        Self {
            protocol,
            compat_flags: None,
            prev_name: Vec::new(),
            prev_mode: 0,
            prev_mtime: 0,
            prev_uid: 0,
            prev_gid: 0,
        }
    }

    /// Creates a new file list reader with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        Self {
            protocol,
            compat_flags: Some(compat_flags),
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
    ///
    /// If the sender transmitted an I/O error marker (SAFE_FILE_LIST mode),
    /// returns an `InvalidData` error with the message "file list I/O error: N"
    /// where N is the error code from the sender.
    pub fn read_entry<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<Option<FileEntry>> {
        // Read flags (as varint if VARINT_FLIST_FLAGS set, as byte otherwise)
        //
        // IMPORTANT: The use of varint encoding for file list flags is controlled by
        // VARINT_FLIST_FLAGS in compat_flags, NOT by protocol version alone.
        // The server only sets this flag if the client advertises 'v' capability.
        // See upstream compat.c: strchr(client_info, 'v') != NULL
        //
        // If client didn't send 'v' capability, compat_flags won't include
        // VARINT_FLIST_FLAGS, and the file list uses single-byte flags even for
        // protocol 30+. This is critical for daemon client interop!
        static FLIST_ENTRY_COUNT: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let entry_idx = FLIST_ENTRY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let use_varint_flags = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
        let _ = std::fs::write(
            format!("/tmp/flist_{entry_idx:03}_READ_ENTRY"),
            format!(
                "use_varint={} prev_name_len={}",
                use_varint_flags,
                self.prev_name.len()
            ),
        );

        // Read primary flags byte
        let flags_byte = if use_varint_flags {
            // Varint encoding: read as varint, primary flags in low 8 bits
            let v = read_varint(reader)?;
            let _ = std::fs::write(
                format!("/tmp/flist_{entry_idx:03}_FLAGS"),
                format!("varint={v:#x}"),
            );
            v
        } else {
            // Single-byte encoding
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            let _ = std::fs::write(
                format!("/tmp/flist_{entry_idx:03}_FLAGS"),
                format!("byte={:#x} binary={:08b}", buf[0], buf[0]),
            );
            buf[0] as i32
        };

        // Zero value marks end of file list
        if flags_byte == 0 {
            return Ok(None);
        }

        // Read extended flags byte if XMIT_EXTENDED_FLAGS is set and NOT using varint
        // For varint, extended flags are in bits 8-15 of the varint value.
        // For non-varint (protocol 28-29 or no VARINT_FLIST_FLAGS), extended flags
        // are a separate second byte read ONLY when XMIT_EXTENDED_FLAGS is set.
        let ext_byte = if use_varint_flags {
            // Extended flags are in bits 8-15 of the varint
            ((flags_byte >> 8) & 0xFF) as u8
        } else if (flags_byte as u8 & XMIT_EXTENDED_FLAGS) != 0 {
            // Read separate extended flags byte
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            let _ = std::fs::write(
                format!("/tmp/flist_{entry_idx:03}_EXT_FLAGS"),
                format!("byte={:#x}", buf[0]),
            );
            buf[0]
        } else {
            0
        };

        // Convert primary flags to u8 for further processing
        let flags_byte = flags_byte as u8;

        // Reconstruct combined flags value for error marker check
        let flags_value = (flags_byte as i32) | ((ext_byte as i32) << 8);

        // Check for I/O error endlist marker (upstream flist.c:2633)
        // flags == (XMIT_EXTENDED_FLAGS|XMIT_IO_ERROR_ENDLIST)
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        if flags_value == error_marker {
            // Check if safe file list mode is enabled (compat.c:775)
            let use_safe_inc_flist = if let Some(flags) = self.compat_flags {
                flags.contains(CompatibilityFlags::SAFE_FILE_LIST)
            } else {
                false
            } || self.protocol.as_u8() >= 31;

            if !use_safe_inc_flist {
                // Protocol error: received error marker without safe mode
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid flist flag: {flags_value:#x}"),
                ));
            }

            // Read error code from sender
            let error_code = read_varint(reader)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("file list I/O error: {error_code}"),
            ));
        }

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

        // Read nanoseconds if XMIT_MOD_NSEC is set (protocol 31+)
        // Upstream flist.c:597-598: read_varint(f) if mtime has nsec
        let _nsec = if flags.mod_nsec() {
            crate::read_varint(reader)? as u32
        } else {
            0
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
        static NAME_READ_COUNT: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let name_idx = NAME_READ_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let _ = std::fs::write(
            format!("/tmp/name_{name_idx:03}_START"),
            format!(
                "same_name={} long_name={} prev_name_len={}",
                flags.same_name(),
                flags.long_name(),
                self.prev_name.len()
            ),
        );

        // Determine how many bytes are shared with the previous name
        let same_len = if flags.same_name() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            let _ = std::fs::write(
                format!("/tmp/name_{name_idx:03}_SAME_LEN"),
                format!("byte={:#x} ({})", byte[0], byte[0]),
            );
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
            let _ = std::fs::write(
                format!("/tmp/name_{name_idx:03}_SUFFIX_LEN"),
                format!("byte={:#x} ({})", byte[0], byte[0]),
            );
            byte[0] as usize
        };

        // Validate lengths
        if same_len > self.prev_name.len() {
            let _ = std::fs::write(
                format!("/tmp/name_{name_idx:03}_ERROR"),
                format!(
                    "same_len={} > prev_name_len={}",
                    same_len,
                    self.prev_name.len()
                ),
            );
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
            let _ = std::fs::write(
                format!("/tmp/name_{name_idx:03}_SUFFIX_BYTES"),
                format!(
                    "bytes={:?} str={:?}",
                    &name[start..],
                    String::from_utf8_lossy(&name[start..])
                ),
            );
        }

        let _ = std::fs::write(
            format!("/tmp/name_{name_idx:03}_RESULT"),
            format!("name={:?}", String::from_utf8_lossy(&name)),
        );

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

    #[test]
    fn read_entry_detects_error_marker_with_safe_file_list() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = test_protocol();
        // Need VARINT_FLIST_FLAGS to read varint-encoded data
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        // Construct error marker: XMIT_EXTENDED_FLAGS | (XMIT_IO_ERROR_ENDLIST << 8)
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        let error_code = 42;

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);
        encode_varint_to_vec(error_code, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err(), "should return error");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("file list I/O error: 42"),
            "error message should contain error code"
        );
    }

    #[test]
    fn read_entry_rejects_error_marker_without_safe_file_list() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        // Use protocol 30 to avoid automatic safe mode (protocol >= 31)
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        // Need VARINT_FLIST_FLAGS to read varint-encoded data, but NOT SAFE_FILE_LIST
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        // Construct error marker
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err(), "should return protocol error");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("Invalid flist flag"),
            "error message should indicate invalid flag"
        );
    }

    #[test]
    fn read_entry_with_protocol_31_accepts_error_marker() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        // Need VARINT_FLIST_FLAGS to read varint-encoded data
        // Protocol 31+ automatically enables safe file list mode
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        // Construct error marker + error code
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        let error_code = 99;

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);
        encode_varint_to_vec(error_code, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        // Protocol 31+ should accept error marker
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("file list I/O error: 99"));
    }

    #[test]
    fn read_write_round_trip_with_safe_file_list_error_nonvarint() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        // Test non-varint mode (protocol 30 without VARINT_FLIST_FLAGS)
        // In non-varint mode, the error marker (XMIT_EXTENDED_FLAGS|XMIT_IO_ERROR_ENDLIST)
        // is written and causes read_entry to return an error.
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let flags = CompatibilityFlags::SAFE_FILE_LIST; // Note: no VARINT_FLIST_FLAGS

        // Write error marker
        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut data = Vec::new();
        writer.write_end(&mut data, Some(123)).unwrap();

        // Read error marker
        let mut reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err(), "expected error, got: {result:?}");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("file list I/O error: 123"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn read_write_round_trip_with_varint_end_marker() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;
        use crate::varint::read_varint;

        // Test varint mode (VARINT_FLIST_FLAGS enabled)
        // In varint mode, end-of-list is 0 followed by error code varint.
        // read_entry returns Ok(None) and caller reads error code separately.
        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;

        // Write end marker with error code
        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut data = Vec::new();
        writer.write_end(&mut data, Some(123)).unwrap();

        // Read end marker - returns Ok(None)
        let mut reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "end marker should return None");

        // Read the error code that follows the end marker
        let error_code = read_varint(&mut cursor).unwrap();
        assert_eq!(
            error_code, 123,
            "error code should be readable after end marker"
        );
    }
}
