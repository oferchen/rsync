//! Wire format generator for file list integration tests.
//!
//! This module provides utilities to generate valid rsync wire format data
//! for file list entries, enabling integration tests without requiring a
//! real rsync sender.

use std::io::{Cursor, Write};

/// Protocol version for wire format generation.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolVersion(pub u8);

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self(31) // Default to protocol 31 (modern rsync 3.2+)
    }
}

/// Configuration for file list wire format generation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WireFormatConfig {
    pub protocol: ProtocolVersion,
    pub preserve_times: bool,
    pub preserve_uid: bool,
    pub preserve_gid: bool,
    pub preserve_links: bool,
    pub preserve_perms: bool,
}

impl Default for WireFormatConfig {
    fn default() -> Self {
        Self {
            protocol: ProtocolVersion::default(),
            preserve_times: true,
            preserve_uid: false,
            preserve_gid: false,
            preserve_links: true,
            preserve_perms: true,
        }
    }
}

/// Represents a file entry to be encoded in wire format.
#[derive(Debug, Clone)]
pub struct TestFileEntry {
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub mtime: i64,
    pub is_dir: bool,
    pub link_target: Option<String>,
}

impl TestFileEntry {
    /// Creates a regular file entry.
    pub fn file(name: impl Into<String>, size: u64) -> Self {
        Self {
            name: name.into(),
            size,
            mode: 0o100644,    // regular file with rw-r--r--
            mtime: 1704067200, // 2024-01-01 00:00:00 UTC
            is_dir: false,
            link_target: None,
        }
    }

    /// Creates a directory entry.
    pub fn dir(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: 0,
            mode: 0o040755, // directory with rwxr-xr-x
            mtime: 1704067200,
            is_dir: true,
            link_target: None,
        }
    }

    /// Creates a symlink entry.
    pub fn symlink(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: 0,
            mode: 0o120777, // symlink
            mtime: 1704067200,
            is_dir: false,
            link_target: Some(target.into()),
        }
    }

    /// Sets the file mode.
    #[allow(dead_code)]
    pub fn with_mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the modification time.
    #[allow(dead_code)]
    pub fn with_mtime(mut self, mtime: i64) -> Self {
        self.mtime = mtime;
        self
    }

    /// Sets the file size.
    #[allow(dead_code)]
    pub fn with_size(mut self, size: u64) -> Self {
        self.size = size;
        self
    }
}

/// Wire format flag constants (matching upstream rsync).
mod flags {
    pub const XMIT_TOP_DIR: u8 = 0x01;
    pub const XMIT_SAME_MODE: u8 = 0x02;
    #[allow(dead_code)]
    pub const XMIT_EXTENDED_FLAGS: u8 = 0x04;
    #[allow(dead_code)]
    pub const XMIT_SAME_UID: u8 = 0x08;
    #[allow(dead_code)]
    pub const XMIT_SAME_GID: u8 = 0x10;
    pub const XMIT_SAME_NAME: u8 = 0x20;
    pub const XMIT_LONG_NAME: u8 = 0x40;
    pub const XMIT_SAME_TIME: u8 = 0x80;
}

/// Generates wire format data for a list of file entries.
pub struct WireFormatGenerator {
    config: WireFormatConfig,
    buffer: Cursor<Vec<u8>>,
    prev_name: Vec<u8>,
    prev_mode: u32,
    prev_mtime: i64,
}

impl WireFormatGenerator {
    /// Creates a new wire format generator with the given configuration.
    pub fn new(config: WireFormatConfig) -> Self {
        Self {
            config,
            buffer: Cursor::new(Vec::new()),
            prev_name: Vec::new(),
            prev_mode: 0,
            prev_mtime: 0,
        }
    }

    /// Creates a generator with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(WireFormatConfig::default())
    }

    /// Writes a single file entry to the buffer.
    pub fn write_entry(&mut self, entry: &TestFileEntry) -> std::io::Result<()> {
        let name_bytes = entry.name.as_bytes();

        // Calculate name prefix compression
        let same_len = self.calculate_same_len(name_bytes);
        let suffix = &name_bytes[same_len..];
        let suffix_len = suffix.len();

        // Calculate flags
        let mut primary_flags: u8 = 0;

        if entry.is_dir {
            primary_flags |= flags::XMIT_TOP_DIR;
        }

        if same_len > 0 {
            primary_flags |= flags::XMIT_SAME_NAME;
        }

        if suffix_len > 255 {
            primary_flags |= flags::XMIT_LONG_NAME;
        }

        if entry.mode == self.prev_mode {
            primary_flags |= flags::XMIT_SAME_MODE;
        }

        if entry.mtime == self.prev_mtime {
            primary_flags |= flags::XMIT_SAME_TIME;
        }

        // For modern protocol (31+), use varint encoding for flags
        if self.config.protocol.0 >= 31 {
            self.write_varint(primary_flags as i32)?;
        } else {
            self.buffer.write_all(&[primary_flags])?;
        }

        // Write name
        if same_len > 0 {
            self.buffer.write_all(&[same_len as u8])?;
        }

        if suffix_len > 255 {
            self.write_varint30(suffix_len as u32)?;
        } else {
            self.buffer.write_all(&[suffix_len as u8])?;
        }

        self.buffer.write_all(suffix)?;

        // Write size (varint30)
        self.write_varint30(entry.size as u32)?;

        // Write mtime if different
        if entry.mtime != self.prev_mtime {
            self.write_varlong4(entry.mtime)?;
        }

        // Write mode if different
        if entry.mode != self.prev_mode {
            if self.config.protocol.0 >= 30 {
                self.write_varint(entry.mode as i32)?;
            } else {
                self.buffer.write_all(&(entry.mode as i32).to_le_bytes())?;
            }
        }

        // Write symlink target if present
        if let Some(ref target) = entry.link_target {
            let target_bytes = target.as_bytes();
            self.write_varint30(target_bytes.len() as u32)?;
            self.buffer.write_all(target_bytes)?;
        }

        // Update state for next entry
        self.prev_name = name_bytes.to_vec();
        self.prev_mode = entry.mode;
        self.prev_mtime = entry.mtime;

        Ok(())
    }

    /// Writes the end-of-list marker.
    pub fn write_end_marker(&mut self) -> std::io::Result<()> {
        // Zero byte indicates end of file list
        self.buffer.write_all(&[0])
    }

    /// Returns the generated wire format data.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buffer.into_inner()
    }

    /// Calculates the common prefix length between current and previous name.
    fn calculate_same_len(&self, name: &[u8]) -> usize {
        let max_len = std::cmp::min(255, std::cmp::min(name.len(), self.prev_name.len()));

        name.iter()
            .zip(self.prev_name.iter())
            .take(max_len)
            .take_while(|(a, b)| a == b)
            .count()
    }

    /// Writes a varint (variable-length integer).
    fn write_varint(&mut self, value: i32) -> std::io::Result<()> {
        let mut v = value as u32;
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                self.buffer.write_all(&[byte])?;
                break;
            } else {
                self.buffer.write_all(&[byte | 0x80])?;
            }
        }
        Ok(())
    }

    /// Writes a varint30 (30-bit variable-length integer used for sizes).
    fn write_varint30(&mut self, value: u32) -> std::io::Result<()> {
        if value < 0xC0 {
            self.buffer.write_all(&[value as u8])?;
        } else if value < 0x4000 {
            let b0 = ((value >> 8) | 0xC0) as u8;
            let b1 = (value & 0xFF) as u8;
            self.buffer.write_all(&[b0, b1])?;
        } else if value < 0x200000 {
            let b0 = ((value >> 16) | 0xE0) as u8;
            let b1 = ((value >> 8) & 0xFF) as u8;
            let b2 = (value & 0xFF) as u8;
            self.buffer.write_all(&[b0, b1, b2])?;
        } else {
            let b0 = 0xF0u8;
            self.buffer.write_all(&[b0])?;
            self.buffer.write_all(&value.to_le_bytes())?;
        }
        Ok(())
    }

    /// Writes a varlong4 (variable-length long integer).
    fn write_varlong4(&mut self, value: i64) -> std::io::Result<()> {
        // For simplicity, write as 4-byte LE for most common case
        self.buffer.write_all(&(value as i32).to_le_bytes())
    }
}

/// Generates wire format data for a flat directory with N files.
pub fn generate_flat_directory(num_files: usize) -> Vec<u8> {
    let mut writer = WireFormatGenerator::with_defaults();

    for i in 0..num_files {
        let entry = TestFileEntry::file(format!("file{i:04}.txt"), 100 + i as u64);
        writer.write_entry(&entry).expect("write entry");
    }

    writer.write_end_marker().expect("write end marker");
    writer.into_bytes()
}

/// Generates wire format data for a nested directory structure.
pub fn generate_nested_directories(depth: usize, files_per_dir: usize) -> Vec<u8> {
    let mut writer = WireFormatGenerator::with_defaults();
    let mut path = String::new();

    for d in 0..depth {
        // Write directory entry
        if d > 0 {
            path.push('/');
        }
        path.push_str(&format!("dir{d}"));

        let dir_entry = TestFileEntry::dir(&path);
        writer.write_entry(&dir_entry).expect("write dir");

        // Write files in this directory
        for f in 0..files_per_dir {
            let file_path = format!("{path}/file{f}.txt");
            let file_entry = TestFileEntry::file(file_path, 100);
            writer.write_entry(&file_entry).expect("write file");
        }
    }

    writer.write_end_marker().expect("write end marker");
    writer.into_bytes()
}

/// Generates wire format data with entries out of order (child before parent).
pub fn generate_out_of_order_entries() -> Vec<u8> {
    let mut writer = WireFormatGenerator::with_defaults();

    // File in subdirectory (child) - written before parent
    writer
        .write_entry(&TestFileEntry::file("parent/child/file.txt", 100))
        .expect("write file");

    // Parent directory - written after child
    writer
        .write_entry(&TestFileEntry::dir("parent"))
        .expect("write parent");

    // Another file at root level
    writer
        .write_entry(&TestFileEntry::file("root_file.txt", 200))
        .expect("write root file");

    writer.write_end_marker().expect("write end marker");
    writer.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_flat_directory() {
        let data = generate_flat_directory(10);
        assert!(!data.is_empty());
        // Should end with zero byte (end marker)
        assert_eq!(*data.last().unwrap(), 0);
    }

    #[test]
    fn test_generate_nested_directories() {
        let data = generate_nested_directories(3, 2);
        assert!(!data.is_empty());
        assert_eq!(*data.last().unwrap(), 0);
    }

    #[test]
    fn test_generate_out_of_order() {
        let data = generate_out_of_order_entries();
        assert!(!data.is_empty());
        assert_eq!(*data.last().unwrap(), 0);
    }

    #[test]
    fn test_file_entry_builders() {
        let file = TestFileEntry::file("test.txt", 100);
        assert_eq!(file.name, "test.txt");
        assert_eq!(file.size, 100);
        assert!(!file.is_dir);

        let dir = TestFileEntry::dir("testdir");
        assert!(dir.is_dir);
        assert_eq!(dir.size, 0);

        let link = TestFileEntry::symlink("link", "target");
        assert!(link.link_target.is_some());
    }

    #[test]
    fn test_varint30_encoding() {
        let mut writer = WireFormatGenerator::with_defaults();

        // Small value (< 0xC0)
        writer.write_varint30(0x3F).unwrap();
        assert_eq!(writer.buffer.get_ref().len(), 1);

        // Medium value (< 0x4000)
        writer.write_varint30(0x3FFF).unwrap();
        // Total should be 1 + 2 = 3 bytes
        assert_eq!(writer.buffer.get_ref().len(), 3);
    }
}
