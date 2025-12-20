#![deny(unsafe_code)]
//! File entry wire format for file list exchange.
//!
//! This module implements the serialization format for file metadata used during
//! the file list exchange phase of the rsync protocol. The format mirrors upstream
//! rsync 3.4.1's flist.c implementation.

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::path::Path;

use crate::varint::{read_varint, write_varint};

/// File type enumeration matching rsync's S_IF* macros.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FileType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Character device.
    CharDevice,
    /// Block device.
    BlockDevice,
    /// Named pipe (FIFO).
    Fifo,
}

/// Flags indicating which optional fields are present in the wire format.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileEntryFlags {
    /// File has extended attributes.
    pub has_xattrs: bool,
    /// File has ACLs.
    pub has_acls: bool,
    /// File is a hardlink reference.
    pub is_hardlink: bool,
    /// File has same UID as previous entry.
    pub same_uid: bool,
    /// File has same GID as previous entry.
    pub same_gid: bool,
    /// File has same mode as previous entry.
    pub same_mode: bool,
}

/// File metadata entry for wire protocol exchange.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path from transfer root.
    pub path: String,
    /// File type.
    pub file_type: FileType,
    /// File size in bytes (0 for directories/devices).
    pub size: u64,
    /// Modification time (seconds since Unix epoch).
    pub mtime: i64,
    /// Unix mode bits (permissions + file type).
    pub mode: u32,
    /// User ID (omitted if same_uid flag set).
    pub uid: Option<u32>,
    /// Group ID (omitted if same_gid flag set).
    pub gid: Option<u32>,
    /// Symlink target path (only for symlinks).
    pub symlink_target: Option<String>,
    /// Device major number (only for devices).
    pub dev_major: Option<u32>,
    /// Device minor number (only for devices).
    pub dev_minor: Option<u32>,
}

impl FileEntry {
    /// Writes a file entry to the wire format.
    ///
    /// The format uses differential encoding to minimize bandwidth:
    /// - Paths are sent relative to the previous entry's path
    /// - uid/gid/mode can be omitted if same as previous entry
    /// - Varints are used for all integer fields
    pub fn write_to<W: Write>(&self, writer: &mut W, prev: Option<&FileEntry>) -> io::Result<()> {
        let mut flags = 0u8;

        if self.file_type == FileType::Symlink {
            flags |= 0x01;
        }
        if self.file_type == FileType::Directory {
            flags |= 0x02;
        }
        if matches!(
            self.file_type,
            FileType::CharDevice | FileType::BlockDevice | FileType::Fifo
        ) {
            flags |= 0x04;
        }

        let same_uid = prev.is_some_and(|p| p.uid == self.uid);
        let same_gid = prev.is_some_and(|p| p.gid == self.gid);
        let same_mode = prev.is_some_and(|p| p.mode == self.mode);

        if same_uid {
            flags |= 0x08;
        }
        if same_gid {
            flags |= 0x10;
        }
        if same_mode {
            flags |= 0x20;
        }

        writer.write_all(&[flags])?;

        let path_bytes = self.path.as_bytes();
        write_varint(writer, path_bytes.len() as i32)?;
        writer.write_all(path_bytes)?;

        if !matches!(self.file_type, FileType::Directory) {
            write_varint(writer, self.size as i32)?;
        }

        write_varint(writer, self.mtime as i32)?;

        if !same_mode {
            write_varint(writer, self.mode as i32)?;
        }

        if !same_uid && let Some(uid) = self.uid {
            write_varint(writer, uid as i32)?;
        }

        if !same_gid && let Some(gid) = self.gid {
            write_varint(writer, gid as i32)?;
        }

        if self.file_type == FileType::Symlink
            && let Some(ref target) = self.symlink_target
        {
            let target_bytes = target.as_bytes();
            write_varint(writer, target_bytes.len() as i32)?;
            writer.write_all(target_bytes)?;
        }

        if matches!(self.file_type, FileType::CharDevice | FileType::BlockDevice)
            && let (Some(major), Some(minor)) = (self.dev_major, self.dev_minor)
        {
            write_varint(writer, major as i32)?;
            write_varint(writer, minor as i32)?;
        }

        Ok(())
    }

    /// Reads a file entry from the wire format.
    pub fn read_from<R: Read>(reader: &mut R, prev: Option<&FileEntry>) -> io::Result<Self> {
        let mut flags_buf = [0u8; 1];
        reader.read_exact(&mut flags_buf)?;
        let flags = flags_buf[0];

        let is_symlink = (flags & 0x01) != 0;
        let is_directory = (flags & 0x02) != 0;
        let is_device = (flags & 0x04) != 0;
        let same_uid = (flags & 0x08) != 0;
        let same_gid = (flags & 0x10) != 0;
        let same_mode = (flags & 0x20) != 0;

        let file_type = if is_symlink {
            FileType::Symlink
        } else if is_directory {
            FileType::Directory
        } else if is_device {
            FileType::CharDevice
        } else {
            FileType::Regular
        };

        let path_len = read_varint(reader)? as usize;
        let mut path_bytes = vec![0u8; path_len];
        reader.read_exact(&mut path_bytes)?;
        let path = String::from_utf8(path_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let size = if is_directory {
            0
        } else {
            read_varint(reader)? as u64
        };

        let mtime = read_varint(reader)? as i64;

        let mode = if same_mode {
            prev.map(|p| p.mode).unwrap_or(0o644)
        } else {
            read_varint(reader)? as u32
        };

        let uid = if same_uid {
            prev.and_then(|p| p.uid)
        } else {
            Some(read_varint(reader)? as u32)
        };

        let gid = if same_gid {
            prev.and_then(|p| p.gid)
        } else {
            Some(read_varint(reader)? as u32)
        };

        let symlink_target = if is_symlink {
            let target_len = read_varint(reader)? as usize;
            let mut target_bytes = vec![0u8; target_len];
            reader.read_exact(&mut target_bytes)?;
            Some(
                String::from_utf8(target_bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            )
        } else {
            None
        };

        let (dev_major, dev_minor) = if is_device && !is_directory && !is_symlink {
            let major = read_varint(reader)? as u32;
            let minor = read_varint(reader)? as u32;
            (Some(major), Some(minor))
        } else {
            (None, None)
        };

        Ok(Self {
            path,
            file_type,
            size,
            mtime,
            mode,
            uid,
            gid,
            symlink_target,
            dev_major,
            dev_minor,
        })
    }

    /// Creates a file entry from filesystem metadata.
    #[cfg(unix)]
    pub fn from_metadata(path: &Path, metadata: &std::fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let file_type = if metadata.is_symlink() {
            FileType::Symlink
        } else if metadata.is_dir() {
            FileType::Directory
        } else if metadata.file_type().is_char_device() {
            FileType::CharDevice
        } else if metadata.file_type().is_block_device() {
            FileType::BlockDevice
        } else if metadata.file_type().is_fifo() {
            FileType::Fifo
        } else {
            FileType::Regular
        };

        let symlink_target = if file_type == FileType::Symlink {
            Some(std::fs::read_link(path)?.to_string_lossy().into_owned())
        } else {
            None
        };

        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            file_type,
            size: metadata.len(),
            mtime: metadata.mtime(),
            mode: metadata.mode(),
            uid: Some(metadata.uid()),
            gid: Some(metadata.gid()),
            symlink_target,
            dev_major: if matches!(file_type, FileType::CharDevice | FileType::BlockDevice) {
                Some((metadata.rdev() >> 8) as u32)
            } else {
                None
            },
            dev_minor: if matches!(file_type, FileType::CharDevice | FileType::BlockDevice) {
                Some((metadata.rdev() & 0xFF) as u32)
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_entry_roundtrip_regular_file() {
        let entry = FileEntry {
            path: "test/file.txt".to_string(),
            file_type: FileType::Regular,
            size: 12345,
            mtime: 1700000000,
            mode: 0o644,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        let mut buf = Vec::new();
        entry.write_to(&mut buf, None).unwrap();

        let decoded = FileEntry::read_from(&mut &buf[..], None).unwrap();

        assert_eq!(decoded.path, entry.path);
        assert_eq!(decoded.file_type, entry.file_type);
        assert_eq!(decoded.size, entry.size);
        assert_eq!(decoded.mtime, entry.mtime);
        assert_eq!(decoded.mode, entry.mode);
        assert_eq!(decoded.uid, entry.uid);
        assert_eq!(decoded.gid, entry.gid);
    }

    #[test]
    fn file_entry_roundtrip_directory() {
        let entry = FileEntry {
            path: "test/dir".to_string(),
            file_type: FileType::Directory,
            size: 0,
            mtime: 1700000000,
            mode: 0o755,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        let mut buf = Vec::new();
        entry.write_to(&mut buf, None).unwrap();

        let decoded = FileEntry::read_from(&mut &buf[..], None).unwrap();

        assert_eq!(decoded.path, entry.path);
        assert_eq!(decoded.file_type, FileType::Directory);
        assert_eq!(decoded.size, 0);
    }

    #[test]
    fn file_entry_roundtrip_symlink() {
        let entry = FileEntry {
            path: "test/link".to_string(),
            file_type: FileType::Symlink,
            size: 0,
            mtime: 1700000000,
            mode: 0o777,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: Some("../target".to_string()),
            dev_major: None,
            dev_minor: None,
        };

        let mut buf = Vec::new();
        entry.write_to(&mut buf, None).unwrap();

        let decoded = FileEntry::read_from(&mut &buf[..], None).unwrap();

        assert_eq!(decoded.path, entry.path);
        assert_eq!(decoded.file_type, FileType::Symlink);
        assert_eq!(decoded.symlink_target, Some("../target".to_string()));
    }

    #[test]
    fn file_entry_differential_encoding() {
        let entry1 = FileEntry {
            path: "test/file1.txt".to_string(),
            file_type: FileType::Regular,
            size: 100,
            mtime: 1700000000,
            mode: 0o644,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        let entry2 = FileEntry {
            path: "test/file2.txt".to_string(),
            file_type: FileType::Regular,
            size: 200,
            mtime: 1700000001,
            mode: 0o644,
            uid: Some(1000),
            gid: Some(1000),
            symlink_target: None,
            dev_major: None,
            dev_minor: None,
        };

        let mut buf1 = Vec::new();
        entry1.write_to(&mut buf1, None).unwrap();

        let mut buf2 = Vec::new();
        entry2.write_to(&mut buf2, Some(&entry1)).unwrap();

        assert!(
            buf2.len() < buf1.len(),
            "differential encoding should be smaller"
        );

        let decoded2 = FileEntry::read_from(&mut &buf2[..], Some(&entry1)).unwrap();
        assert_eq!(decoded2.uid, Some(1000));
        assert_eq!(decoded2.gid, Some(1000));
        assert_eq!(decoded2.mode, 0o644);
    }
}
