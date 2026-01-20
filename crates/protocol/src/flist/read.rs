//! File list reading (decoding) from the rsync wire format.
//!
//! This module implements the receiver side of file list exchange, decoding
//! file entries as they arrive from the sender.

use std::io::{self, Read};
use std::path::PathBuf;

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::codec::{ProtocolCodec, ProtocolCodecEnum, create_protocol_codec};
use crate::iconv::FilenameConverter;
use crate::varint::{read_varint, read_varint30_int};

use super::entry::FileEntry;
use super::flags::{
    FileFlags, XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST,
};
use super::state::FileListCompressionState;

/// Result of reading flags from the wire.
#[derive(Debug)]
enum FlagsResult {
    /// End of file list reached (zero flags byte).
    EndOfList,
    /// I/O error marker with error code from sender.
    IoError(i32),
    /// Valid flags for a file entry.
    Flags(FileFlags),
}

/// State maintained while reading a file list.
///
/// The rsync protocol uses compression across entries, where fields that match
/// the previous entry are omitted. This reader maintains the necessary state
/// to decode these compressed entries.
#[derive(Debug)]
pub struct FileListReader {
    /// Protocol version being used.
    protocol: ProtocolVersion,
    /// Protocol codec for version-aware encoding/decoding.
    codec: ProtocolCodecEnum,
    /// Compatibility flags for this session.
    compat_flags: Option<CompatibilityFlags>,
    /// Compression state for cross-entry field sharing.
    state: FileListCompressionState,
    /// Whether to preserve (and thus read) UID values from the wire.
    preserve_uid: bool,
    /// Whether to preserve (and thus read) GID values from the wire.
    preserve_gid: bool,
    /// Whether to preserve (and thus read) symlink targets from the wire.
    preserve_links: bool,
    /// Whether to preserve (and thus read) device numbers from the wire.
    preserve_devices: bool,
    /// Whether to preserve (and thus read) hardlink indices from the wire.
    preserve_hard_links: bool,
    /// Optional filename encoding converter (for --iconv support).
    iconv: Option<FilenameConverter>,
}

/// Result from reading metadata fields.
struct MetadataResult {
    mtime: i64,
    mode: u32,
    user_name: Option<String>,
    group_name: Option<String>,
}

impl FileListReader {
    /// Creates a new file list reader for the given protocol version.
    #[must_use]
    pub fn new(protocol: ProtocolVersion) -> Self {
        let codec = create_protocol_codec(protocol.as_u8());
        Self {
            protocol,
            codec,
            compat_flags: None,
            state: FileListCompressionState::new(),
            preserve_uid: false,
            preserve_gid: false,
            preserve_links: false,
            preserve_devices: false,
            preserve_hard_links: false,
            iconv: None,
        }
    }

    /// Creates a new file list reader with compatibility flags.
    #[must_use]
    pub fn with_compat_flags(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        let codec = create_protocol_codec(protocol.as_u8());
        Self {
            protocol,
            codec,
            compat_flags: Some(compat_flags),
            state: FileListCompressionState::new(),
            preserve_uid: false,
            preserve_gid: false,
            preserve_links: false,
            preserve_devices: false,
            preserve_hard_links: false,
            iconv: None,
        }
    }

    /// Sets whether UID values should be read from the wire.
    #[must_use]
    pub const fn with_preserve_uid(mut self, preserve: bool) -> Self {
        self.preserve_uid = preserve;
        self
    }

    /// Sets whether GID values should be read from the wire.
    #[must_use]
    pub const fn with_preserve_gid(mut self, preserve: bool) -> Self {
        self.preserve_gid = preserve;
        self
    }

    /// Sets whether symlink targets should be read from the wire.
    #[must_use]
    pub const fn with_preserve_links(mut self, preserve: bool) -> Self {
        self.preserve_links = preserve;
        self
    }

    /// Sets whether device numbers should be read from the wire.
    #[must_use]
    pub const fn with_preserve_devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Sets whether hardlink indices should be read from the wire.
    #[must_use]
    pub const fn with_preserve_hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
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

    /// Reads and validates flags from the wire.
    ///
    /// Returns `FlagsResult::EndOfList` for end-of-list marker,
    /// `FlagsResult::IoError` for I/O error markers, or
    /// `FlagsResult::Flags` for valid entry flags.
    fn read_flags<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<FlagsResult> {
        let use_varint = self.use_varint_flags();

        // Read primary flags
        let flags_value = if use_varint {
            read_varint(reader)?
        } else {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0] as i32
        };

        // Check for end-of-list marker
        if flags_value == 0 {
            if use_varint {
                // In varint mode, error code follows zero flags
                let io_error = read_varint(reader)?;
                if io_error != 0 {
                    return Ok(FlagsResult::IoError(io_error));
                }
            }
            return Ok(FlagsResult::EndOfList);
        }

        // Read extended flags
        let ext_byte = if use_varint {
            ((flags_value >> 8) & 0xFF) as u8
        } else if (flags_value as u8 & XMIT_EXTENDED_FLAGS) != 0 {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0]
        } else {
            0
        };

        let primary_byte = flags_value as u8;

        // Check for I/O error marker
        if let Some(error) = self.check_error_marker(primary_byte, ext_byte, reader)? {
            return Ok(FlagsResult::IoError(error));
        }

        // Build flags structure
        let flags = if ext_byte != 0 || (primary_byte & XMIT_EXTENDED_FLAGS) != 0 {
            FileFlags::new(primary_byte, ext_byte)
        } else {
            FileFlags::new(primary_byte, 0)
        };

        Ok(FlagsResult::Flags(flags))
    }

    /// Checks for I/O error marker in flags.
    ///
    /// Returns `Some(error_code)` if an error marker is detected,
    /// `None` if flags represent a valid entry.
    fn check_error_marker<R: Read + ?Sized>(
        &self,
        primary: u8,
        extended: u8,
        reader: &mut R,
    ) -> io::Result<Option<i32>> {
        let flags_value = (primary as i32) | ((extended as i32) << 8);
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

        if flags_value != error_marker {
            return Ok(None);
        }

        if !self.use_safe_file_list() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid flist flag: {flags_value:#x}"),
            ));
        }

        let error_code = read_varint(reader)?;
        Ok(Some(error_code))
    }

    /// Reads metadata fields (mtime, nsec, mode, uid, gid, user_name, group_name) based on flags.
    fn read_metadata<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<MetadataResult> {
        // Read mtime
        let mtime = if flags.same_time() {
            self.state.prev_mtime
        } else {
            let mtime = crate::read_varlong(reader, 4)?;
            self.state.update_mtime(mtime);
            mtime
        };

        // Read nanoseconds if present (protocol 31+)
        let _nsec = if flags.mod_nsec() {
            crate::read_varint(reader)? as u32
        } else {
            0
        };

        // Read mode
        let mode = if flags.same_mode() {
            self.state.prev_mode
        } else {
            let mut mode_bytes = [0u8; 4];
            reader.read_exact(&mut mode_bytes)?;
            let mode = i32::from_le_bytes(mode_bytes) as u32;
            self.state.update_mode(mode);
            mode
        };

        // Read UID and optional user name
        let mut user_name = None;
        if self.preserve_uid && !flags.same_uid() {
            let uid = read_varint(reader)? as u32;
            self.state.update_uid(uid);
            // Read user name if flag set (protocol 30+)
            if flags.user_name_follows() {
                let mut len_buf = [0u8; 1];
                reader.read_exact(&mut len_buf)?;
                let len = len_buf[0] as usize;
                if len > 0 {
                    let mut name_bytes = vec![0u8; len];
                    reader.read_exact(&mut name_bytes)?;
                    user_name = Some(String::from_utf8_lossy(&name_bytes).into_owned());
                }
            }
        }

        // Read GID and optional group name
        let mut group_name = None;
        if self.preserve_gid && !flags.same_gid() {
            let gid = read_varint(reader)? as u32;
            self.state.update_gid(gid);
            // Read group name if flag set (protocol 30+)
            if flags.group_name_follows() {
                let mut len_buf = [0u8; 1];
                reader.read_exact(&mut len_buf)?;
                let len = len_buf[0] as usize;
                if len > 0 {
                    let mut name_bytes = vec![0u8; len];
                    reader.read_exact(&mut name_bytes)?;
                    group_name = Some(String::from_utf8_lossy(&name_bytes).into_owned());
                }
            }
        }

        Ok(MetadataResult {
            mtime,
            mode,
            user_name,
            group_name,
        })
    }

    /// Reads symlink target if preserving links and mode indicates a symlink.
    ///
    /// Wire format: varint30(len) + raw bytes
    fn read_symlink_target<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        mode: u32,
    ) -> io::Result<Option<PathBuf>> {
        // S_IFLNK check: mode & 0o170000 == 0o120000
        let is_symlink = mode & 0o170000 == 0o120000;

        if !self.preserve_links || !is_symlink {
            return Ok(None);
        }

        let len = read_varint30_int(reader, self.protocol.as_u8())? as usize;
        if len == 0 {
            return Ok(None);
        }

        let mut target_bytes = vec![0u8; len];
        reader.read_exact(&mut target_bytes)?;

        // Convert bytes to PathBuf (platform-specific handling)
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let target = std::ffi::OsStr::from_bytes(&target_bytes);
            Ok(Some(PathBuf::from(target)))
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, attempt UTF-8 conversion
            let target_str = String::from_utf8_lossy(&target_bytes);
            Ok(Some(PathBuf::from(target_str.into_owned())))
        }
    }

    /// Reads device numbers if preserving devices and mode indicates a device.
    ///
    /// Wire format (protocol 28+):
    /// - Major: varint30 (omitted if XMIT_SAME_RDEV_MAJOR set)
    /// - Minor: varint (protocol 30+) or byte/int (protocol 28-29)
    fn read_rdev<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        mode: u32,
        flags: FileFlags,
    ) -> io::Result<Option<(u32, u32)>> {
        // S_ISBLK (0o060000) or S_ISCHR (0o020000)
        let type_bits = mode & 0o170000;
        let is_device = type_bits == 0o060000 || type_bits == 0o020000;

        if !self.preserve_devices || !is_device {
            return Ok(None);
        }

        // Read major if not same as previous
        let major = if flags.same_rdev_major() {
            self.state.prev_rdev_major
        } else {
            let m = read_varint30_int(reader, self.protocol.as_u8())? as u32;
            self.state.update_rdev_major(m);
            m
        };

        // Read minor
        let minor = if self.protocol.as_u8() >= 30 {
            read_varint(reader)? as u32
        } else {
            // Protocol 28-29: read byte or int based on XMIT_RDEV_MINOR_8_pre30
            // For simplicity, we check if extended flag bit 3 (0x08) is set
            // This corresponds to XMIT_RDEV_MINOR_8_pre30 in the extended byte
            let minor_is_byte = (flags.extended & 0x08) != 0;
            if minor_is_byte {
                let mut buf = [0u8; 1];
                reader.read_exact(&mut buf)?;
                buf[0] as u32
            } else {
                let mut buf = [0u8; 4];
                reader.read_exact(&mut buf)?;
                i32::from_le_bytes(buf) as u32
            }
        };

        Ok(Some((major, minor)))
    }

    /// Reads hardlink index if preserving hardlinks and flags indicate it.
    ///
    /// Wire format (protocol 30+):
    /// - If XMIT_HLINKED is set but not XMIT_HLINK_FIRST: read varint index
    /// - If XMIT_HLINK_FIRST is also set: return u32::MAX (this is the first/leader)
    fn read_hardlink_idx<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<Option<u32>> {
        if !self.preserve_hard_links || self.protocol.as_u8() < 30 {
            return Ok(None);
        }

        // Check hardlink flags in extended byte
        let hlinked = (flags.extended & XMIT_HLINKED) != 0;
        if !hlinked {
            return Ok(None);
        }

        let hlink_first = (flags.extended & XMIT_HLINK_FIRST) != 0;
        if hlink_first {
            // This is the first/leader of the hardlink group
            return Ok(Some(u32::MAX));
        }

        // Read the index pointing to the leader
        let idx = read_varint(reader)? as u32;
        Ok(Some(idx))
    }

    /// Applies iconv encoding conversion to a filename.
    fn apply_encoding_conversion(&self, name: Vec<u8>) -> io::Result<Vec<u8>> {
        if let Some(ref converter) = self.iconv {
            match converter.remote_to_local(&name) {
                Ok(converted) => Ok(converted.into_owned()),
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("filename encoding conversion failed: {e}"),
                )),
            }
        } else {
            Ok(name)
        }
    }

    /// Returns true if this entry is a hardlink follower (metadata was skipped on wire).
    ///
    /// A hardlink follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST.
    /// Such entries reference another entry in the file list, so their metadata
    /// (size, mtime, mode, uid, gid, symlink, rdev) was omitted from the wire.
    #[inline]
    fn is_hardlink_follower(&self, flags: FileFlags) -> bool {
        flags.hlinked() && !flags.hlink_first()
    }

    /// Reads the next file entry from the stream.
    ///
    /// Returns `None` when the end-of-list marker is received (a zero byte).
    /// Returns an error on I/O failure or malformed data.
    pub fn read_entry<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<Option<FileEntry>> {
        // Step 1: Read and validate flags
        let flags = match self.read_flags(reader)? {
            FlagsResult::EndOfList => return Ok(None),
            FlagsResult::IoError(code) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("file list I/O error: {code}"),
                ));
            }
            FlagsResult::Flags(f) => f,
        };

        // Step 2: Read name with compression
        let name = self.read_name(reader, flags)?;

        // Step 3-6: Read metadata (unless this is a hardlink follower)
        // Hardlink followers have their metadata copied from the leader entry,
        // so we skip reading size, mtime, mode, uid, gid, symlink, and rdev.
        let (size, metadata, link_target, rdev) = if self.is_hardlink_follower(flags) {
            // Use default values for hardlink follower - caller should copy from leader
            (
                0u64,
                MetadataResult {
                    mtime: 0,
                    mode: 0,
                    user_name: None,
                    group_name: None,
                },
                None,
                None,
            )
        } else {
            // Step 3: Read file size
            let size = self.read_size(reader)?;

            // Step 4: Read metadata fields
            let metadata = self.read_metadata(reader, flags)?;

            // Step 5: Read symlink target (if applicable)
            let link_target = self.read_symlink_target(reader, metadata.mode)?;

            // Step 6: Read device numbers (if applicable)
            let rdev = self.read_rdev(reader, metadata.mode, flags)?;

            (size, metadata, link_target, rdev)
        };

        // Step 7: Read hardlink index (if applicable)
        let hardlink_idx = self.read_hardlink_idx(reader, flags)?;

        // Step 8: Apply encoding conversion
        let converted_name = self.apply_encoding_conversion(name)?;

        // Step 9: Construct entry
        let path = PathBuf::from(String::from_utf8_lossy(&converted_name).into_owned());
        let mut entry = FileEntry::from_raw(path, size, metadata.mode, metadata.mtime, 0, flags);

        // Step 10: Set symlink target if present
        if let Some(target) = link_target {
            entry.set_link_target(target);
        }

        // Step 11: Set device numbers if present
        if let Some((major, minor)) = rdev {
            entry.set_rdev(major, minor);
        }

        // Step 12: Set hardlink index if present
        if let Some(idx) = hardlink_idx {
            entry.set_hardlink_idx(idx);
        }

        // Step 13: Set user name if present
        if let Some(name) = metadata.user_name {
            entry.set_user_name(name);
        }

        // Step 14: Set group name if present
        if let Some(name) = metadata.group_name {
            entry.set_group_name(name);
        }

        Ok(Some(entry))
    }

    /// Reads the file name with path compression.
    fn read_name<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<Vec<u8>> {
        // Determine shared prefix length
        let same_len = if flags.same_name() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        } else {
            0
        };

        // Read suffix length
        let suffix_len = if flags.long_name() {
            read_varint(reader)? as usize
        } else {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        };

        // Validate lengths
        if same_len > self.state.prev_name.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "same_len {} exceeds previous name length {}",
                    same_len,
                    self.state.prev_name.len()
                ),
            ));
        }

        // Build full name
        let mut name = Vec::with_capacity(same_len + suffix_len);
        name.extend_from_slice(&self.state.prev_name[..same_len]);

        if suffix_len > 0 {
            let start = name.len();
            name.resize(start + suffix_len, 0);
            reader.read_exact(&mut name[start..])?;
        }

        // Update state
        self.state.update_name(&name);

        Ok(name)
    }

    /// Reads the file size using protocol-appropriate encoding.
    fn read_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<u64> {
        let size = self.codec.read_file_size(reader)?;
        Ok(size as u64)
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
        let data = [0u8];
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

        let mut entry1 = FileEntry::new_file("dir/file".into(), 50, 0o100644);
        entry1.set_mtime(1700000000, 0);

        let mut entry2 = FileEntry::new_file("dir/other".into(), 75, 0o100644);
        entry2.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry1).unwrap();
        writer.write_entry(&mut data, &entry2).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry1.name(), "dir/file");

        let read_entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry2.name(), "dir/other");
    }

    #[test]
    fn read_entry_detects_error_marker_with_safe_file_list() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        let error_code = 42;

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);
        encode_varint_to_vec(error_code, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("file list I/O error: 42"));
    }

    #[test]
    fn read_entry_rejects_error_marker_without_safe_file_list() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("Invalid flist flag"));
    }

    #[test]
    fn read_entry_with_protocol_31_accepts_error_marker() {
        use crate::CompatibilityFlags;
        use crate::varint::encode_varint_to_vec;

        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        let mut reader = FileListReader::with_compat_flags(protocol, flags);

        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
        let error_code = 99;

        let mut data = Vec::new();
        encode_varint_to_vec(error_marker, &mut data);
        encode_varint_to_vec(error_code, &mut data);

        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("file list I/O error: 99"));
    }

    #[test]
    fn read_write_round_trip_with_safe_file_list_error_nonvarint() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let flags = CompatibilityFlags::SAFE_FILE_LIST;

        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut data = Vec::new();
        writer.write_end(&mut data, Some(123)).unwrap();

        let mut reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("file list I/O error: 123"));
    }

    #[test]
    fn read_write_round_trip_with_varint_end_marker() {
        use super::super::write::FileListWriter;
        use crate::CompatibilityFlags;

        let protocol = test_protocol();
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;

        // Test end marker with io_error=0 returns Ok(None)
        let writer = FileListWriter::with_compat_flags(protocol, flags);
        let mut data = Vec::new();
        writer.write_end(&mut data, Some(0)).unwrap();

        let mut reader = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor = Cursor::new(&data[..]);
        let result = reader.read_entry(&mut cursor);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
        assert_eq!(cursor.position() as usize, data.len());

        // Test end marker with non-zero error returns Err
        let mut data2 = Vec::new();
        writer.write_end(&mut data2, Some(123)).unwrap();

        let mut reader2 = FileListReader::with_compat_flags(protocol, flags);
        let mut cursor2 = Cursor::new(&data2[..]);
        let result2 = reader2.read_entry(&mut cursor2);
        assert!(result2.is_err());
        let err = result2.unwrap_err();
        assert!(err.to_string().contains("123"));
    }

    // Tests for extracted helper methods

    #[test]
    fn use_varint_flags_checks_compat_flags() {
        let protocol = test_protocol();

        let reader_without = FileListReader::new(protocol);
        assert!(!reader_without.use_varint_flags());

        let reader_with =
            FileListReader::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
        assert!(reader_with.use_varint_flags());
    }

    #[test]
    fn use_safe_file_list_checks_protocol_and_flags() {
        // Protocol 30 without flag
        let reader30 = FileListReader::new(ProtocolVersion::try_from(30u8).unwrap());
        assert!(!reader30.use_safe_file_list());

        // Protocol 30 with flag
        let reader30_safe = FileListReader::with_compat_flags(
            ProtocolVersion::try_from(30u8).unwrap(),
            CompatibilityFlags::SAFE_FILE_LIST,
        );
        assert!(reader30_safe.use_safe_file_list());

        // Protocol 31+ automatically enables safe mode
        let reader31 = FileListReader::new(ProtocolVersion::try_from(31u8).unwrap());
        assert!(reader31.use_safe_file_list());
    }

    #[test]
    fn read_flags_returns_end_of_list_for_zero() {
        let reader = FileListReader::new(test_protocol());
        let data = [0u8];
        let mut cursor = Cursor::new(&data[..]);

        match reader.read_flags(&mut cursor).unwrap() {
            FlagsResult::EndOfList => {}
            other => panic!("expected EndOfList, got {other:?}"),
        }
    }

    #[test]
    fn read_flags_returns_io_error_in_varint_mode() {
        let reader = FileListReader::with_compat_flags(
            test_protocol(),
            CompatibilityFlags::VARINT_FLIST_FLAGS,
        );

        // Zero flags followed by non-zero error code
        use crate::varint::encode_varint_to_vec;
        let mut data = Vec::new();
        encode_varint_to_vec(0, &mut data); // flags = 0
        encode_varint_to_vec(42, &mut data); // error = 42

        let mut cursor = Cursor::new(&data[..]);

        match reader.read_flags(&mut cursor).unwrap() {
            FlagsResult::IoError(code) => assert_eq!(code, 42),
            other => panic!("expected IoError(42), got {other:?}"),
        }
    }

    #[test]
    fn is_hardlink_follower_helper() {
        use crate::flist::flags::{XMIT_HLINK_FIRST, XMIT_HLINKED};

        let reader = FileListReader::new(test_protocol()).with_preserve_hard_links(true);

        // No hardlink flags
        let flags_none = FileFlags::new(0, 0);
        assert!(!reader.is_hardlink_follower(flags_none));

        // Leader (HLINKED + HLINK_FIRST)
        let flags_leader = FileFlags::new(0, XMIT_HLINKED | XMIT_HLINK_FIRST);
        assert!(!reader.is_hardlink_follower(flags_leader));

        // Follower (HLINKED only, no HLINK_FIRST)
        let flags_follower = FileFlags::new(0, XMIT_HLINKED);
        assert!(reader.is_hardlink_follower(flags_follower));
    }
}
