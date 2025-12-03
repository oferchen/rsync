//! Batch file binary format definitions.
//!
//! This module defines the structures and serialization for the batch file
//! format, maintaining byte-for-byte compatibility with upstream rsync.

use std::io::{self, Read, Write};

/// Batch file header containing protocol negotiation information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchHeader {
    /// Protocol version (i32).
    pub protocol_version: i32,
    /// Compatibility flags (varint for protocol >= 30, None otherwise).
    pub compat_flags: Option<u64>,
    /// Checksum seed for this transfer (i32).
    pub checksum_seed: i32,
    /// Stream flags bitmap (i32).
    pub stream_flags: BatchFlags,
}

impl BatchHeader {
    /// Create a new batch header.
    pub fn new(protocol_version: i32, checksum_seed: i32) -> Self {
        Self {
            protocol_version,
            compat_flags: if protocol_version >= 30 {
                Some(0)
            } else {
                None
            },
            checksum_seed,
            stream_flags: BatchFlags::default(),
        }
    }

    /// Write the header to a writer.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // Write protocol version
        write_i32(writer, self.protocol_version)?;

        // Write compat flags for protocol >= 30
        if let Some(flags) = self.compat_flags {
            write_varint(writer, flags)?;
        }

        // Write checksum seed
        write_i32(writer, self.checksum_seed)?;

        // Write stream flags
        self.stream_flags.write_to(writer)?;

        Ok(())
    }

    /// Read the header from a reader.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        // Read protocol version
        let protocol_version = read_i32(reader)?;

        // Read compat flags for protocol >= 30
        let compat_flags = if protocol_version >= 30 {
            Some(read_varint(reader)?)
        } else {
            None
        };

        // Read checksum seed
        let checksum_seed = read_i32(reader)?;

        // Read stream flags
        let stream_flags = BatchFlags::read_from(reader)?;

        Ok(Self {
            protocol_version,
            compat_flags,
            checksum_seed,
            stream_flags,
        })
    }
}

/// Stream flags bitmap that affects data stream format.
///
/// These flags must match between write and read to ensure correct
/// interpretation of the batch file. The flag positions match upstream rsync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchFlags {
    /// Bit 0: --recurse (-r)
    pub recurse: bool,
    /// Bit 1: --owner (-o)
    pub preserve_uid: bool,
    /// Bit 2: --group (-g)
    pub preserve_gid: bool,
    /// Bit 3: --links (-l)
    pub preserve_links: bool,
    /// Bit 4: --devices (-D)
    pub preserve_devices: bool,
    /// Bit 5: --hard-links (-H)
    pub preserve_hard_links: bool,
    /// Bit 6: --checksum (-c)
    pub always_checksum: bool,
    /// Bit 7: --dirs (-d) [protocol >= 29]
    pub xfer_dirs: bool,
    /// Bit 8: --compress (-z) [protocol >= 29]
    pub do_compression: bool,
    /// Bit 9: --iconv [protocol >= 30]
    pub iconv: bool,
    /// Bit 10: --acls (-A) [protocol >= 30]
    pub preserve_acls: bool,
    /// Bit 11: --xattrs (-X) [protocol >= 30]
    pub preserve_xattrs: bool,
    /// Bit 12: --inplace [protocol >= 30]
    pub inplace: bool,
    /// Bit 13: --append [protocol >= 30]
    pub append: bool,
    /// Bit 14: --append-verify [protocol >= 30]
    pub append_verify: bool,
}

impl BatchFlags {
    /// Create a new flags structure from a bitmap.
    pub fn from_bitmap(bitmap: i32, protocol_version: i32) -> Self {
        let mut flags = Self::default();
        flags.recurse = (bitmap & (1 << 0)) != 0;
        flags.preserve_uid = (bitmap & (1 << 1)) != 0;
        flags.preserve_gid = (bitmap & (1 << 2)) != 0;
        flags.preserve_links = (bitmap & (1 << 3)) != 0;
        flags.preserve_devices = (bitmap & (1 << 4)) != 0;
        flags.preserve_hard_links = (bitmap & (1 << 5)) != 0;
        flags.always_checksum = (bitmap & (1 << 6)) != 0;

        if protocol_version >= 29 {
            flags.xfer_dirs = (bitmap & (1 << 7)) != 0;
            flags.do_compression = (bitmap & (1 << 8)) != 0;
        }

        if protocol_version >= 30 {
            flags.iconv = (bitmap & (1 << 9)) != 0;
            flags.preserve_acls = (bitmap & (1 << 10)) != 0;
            flags.preserve_xattrs = (bitmap & (1 << 11)) != 0;
            flags.inplace = (bitmap & (1 << 12)) != 0;
            flags.append = (bitmap & (1 << 13)) != 0;
            flags.append_verify = (bitmap & (1 << 14)) != 0;
        }

        flags
    }

    /// Convert flags to a bitmap.
    pub fn to_bitmap(&self, protocol_version: i32) -> i32 {
        let mut bitmap = 0i32;

        if self.recurse {
            bitmap |= 1 << 0;
        }
        if self.preserve_uid {
            bitmap |= 1 << 1;
        }
        if self.preserve_gid {
            bitmap |= 1 << 2;
        }
        if self.preserve_links {
            bitmap |= 1 << 3;
        }
        if self.preserve_devices {
            bitmap |= 1 << 4;
        }
        if self.preserve_hard_links {
            bitmap |= 1 << 5;
        }
        if self.always_checksum {
            bitmap |= 1 << 6;
        }

        if protocol_version >= 29 {
            if self.xfer_dirs {
                bitmap |= 1 << 7;
            }
            if self.do_compression {
                bitmap |= 1 << 8;
            }
        }

        if protocol_version >= 30 {
            if self.iconv {
                bitmap |= 1 << 9;
            }
            if self.preserve_acls {
                bitmap |= 1 << 10;
            }
            if self.preserve_xattrs {
                bitmap |= 1 << 11;
            }
            if self.inplace {
                bitmap |= 1 << 12;
            }
            if self.append {
                bitmap |= 1 << 13;
            }
            if self.append_verify {
                bitmap |= 1 << 14;
            }
        }

        bitmap
    }

    /// Write flags to a writer.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // Protocol version is not available here, so we write all flags
        // The caller should ensure protocol-appropriate flags are set
        write_i32(writer, self.to_bitmap(30))
    }

    /// Read flags from a reader.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let bitmap = read_i32(reader)?;
        // Assume protocol 30+ for maximum compatibility
        Ok(Self::from_bitmap(bitmap, 30))
    }
}

/// Write a 32-bit integer in little-endian byte order.
fn write_i32<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Read a 32-bit integer in little-endian byte order.
fn read_i32<R: Read>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Write a variable-length integer (varint) matching upstream rsync's format.
fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> io::Result<()> {
    // Upstream rsync uses a varint format where:
    // - Values < 0x80 use 1 byte
    // - Larger values use multiple bytes with continuation bit
    while value >= 0x80 {
        writer.write_all(&[(value as u8) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])
}

/// Read a variable-length integer (varint) matching upstream rsync's format.
fn read_varint<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        let byte = buf[0];
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "varint overflow",
            ));
        }
    }
    Ok(result)
}

/// Write a variable-length string (length prefix + bytes).
fn write_string<W: Write>(writer: &mut W, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    write_varint(writer, bytes.len() as u64)?;
    writer.write_all(bytes)
}

/// Read a variable-length string (length prefix + bytes).
fn read_string<R: Read>(reader: &mut R) -> io::Result<String> {
    let len = read_varint(reader)? as usize;
    if len > 1024 * 1024 {
        // Sanity check: reject strings > 1 MB
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "string too long",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write a 64-bit unsigned integer in little-endian byte order.
fn write_u64<W: Write>(writer: &mut W, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Read a 64-bit unsigned integer in little-endian byte order.
fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// Write a 32-bit unsigned integer in little-endian byte order.
fn write_u32<W: Write>(writer: &mut W, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Read a 32-bit unsigned integer in little-endian byte order.
fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// File list entry in batch file.
///
/// This structure represents a single file/directory/link entry in the batch
/// file, matching upstream rsync's flist format. The file list is written after
/// the batch header and before the delta operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path from source root.
    pub path: String,
    /// File mode bits (permissions + type).
    pub mode: u32,
    /// File size in bytes.
    pub size: u64,
    /// Modification time (seconds since Unix epoch).
    pub mtime: i64,
    /// Owner user ID (if preserved).
    pub uid: Option<u32>,
    /// Owner group ID (if preserved).
    pub gid: Option<u32>,
}

impl FileEntry {
    /// Create a new file entry.
    pub fn new(path: String, mode: u32, size: u64, mtime: i64) -> Self {
        Self {
            path,
            mode,
            size,
            mtime,
            uid: None,
            gid: None,
        }
    }

    /// Write the file entry to a writer.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // Write path
        write_string(writer, &self.path)?;

        // Write mode
        write_u32(writer, self.mode)?;

        // Write size
        write_u64(writer, self.size)?;

        // Write mtime
        write_i32(writer, self.mtime as i32)?;

        // Write optional uid/gid (varint for space efficiency)
        if let Some(uid) = self.uid {
            write_varint(writer, 1)?; // Flag: uid present
            write_u32(writer, uid)?;
        } else {
            write_varint(writer, 0)?; // Flag: uid not present
        }

        if let Some(gid) = self.gid {
            write_varint(writer, 1)?; // Flag: gid present
            write_u32(writer, gid)?;
        } else {
            write_varint(writer, 0)?; // Flag: gid not present
        }

        Ok(())
    }

    /// Read a file entry from a reader.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        // Read path
        let path = read_string(reader)?;

        // Read mode
        let mode = read_u32(reader)?;

        // Read size
        let size = read_u64(reader)?;

        // Read mtime
        let mtime = read_i32(reader)? as i64;

        // Read optional uid
        let uid = if read_varint(reader)? != 0 {
            Some(read_u32(reader)?)
        } else {
            None
        };

        // Read optional gid
        let gid = if read_varint(reader)? != 0 {
            Some(read_u32(reader)?)
        } else {
            None
        };

        Ok(Self {
            path,
            mode,
            size,
            mtime,
            uid,
            gid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_write_read_i32() {
        let values = [0, 1, -1, i32::MAX, i32::MIN, 12345, -67890];
        for &val in &values {
            let mut buf = Vec::new();
            write_i32(&mut buf, val).unwrap();
            let mut cursor = Cursor::new(buf);
            let read_val = read_i32(&mut cursor).unwrap();
            assert_eq!(val, read_val);
        }
    }

    #[test]
    fn test_write_read_varint() {
        let values = [0, 1, 127, 128, 255, 256, 16383, 16384, u64::MAX];
        for &val in &values {
            let mut buf = Vec::new();
            write_varint(&mut buf, val).unwrap();
            let mut cursor = Cursor::new(buf);
            let read_val = read_varint(&mut cursor).unwrap();
            assert_eq!(val, read_val);
        }
    }

    #[test]
    fn test_batch_flags_bitmap_roundtrip() {
        let mut flags = BatchFlags::default();
        flags.recurse = true;
        flags.preserve_uid = true;
        flags.preserve_links = true;
        flags.preserve_hard_links = true;
        flags.always_checksum = true;

        let bitmap = flags.to_bitmap(30);
        let restored = BatchFlags::from_bitmap(bitmap, 30);
        assert_eq!(flags, restored);
    }

    #[test]
    fn test_batch_flags_protocol_29() {
        let mut flags = BatchFlags::default();
        flags.xfer_dirs = true;
        flags.do_compression = true;

        let bitmap = flags.to_bitmap(29);
        let restored = BatchFlags::from_bitmap(bitmap, 29);
        assert_eq!(flags, restored);

        // Protocol 28 should not include these flags
        let bitmap_28 = flags.to_bitmap(28);
        let restored_28 = BatchFlags::from_bitmap(bitmap_28, 28);
        assert!(!restored_28.xfer_dirs);
        assert!(!restored_28.do_compression);
    }

    #[test]
    fn test_batch_header_write_read() {
        let mut header = BatchHeader::new(30, 12345);
        header.compat_flags = Some(42);
        header.stream_flags.recurse = true;
        header.stream_flags.preserve_uid = true;

        let mut buf = Vec::new();
        header.write_to(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf);
        let restored = BatchHeader::read_from(&mut cursor).unwrap();

        assert_eq!(header.protocol_version, restored.protocol_version);
        assert_eq!(header.compat_flags, restored.compat_flags);
        assert_eq!(header.checksum_seed, restored.checksum_seed);
        assert_eq!(header.stream_flags.recurse, restored.stream_flags.recurse);
        assert_eq!(
            header.stream_flags.preserve_uid,
            restored.stream_flags.preserve_uid
        );
    }

    #[test]
    fn test_batch_header_protocol_28() {
        let header = BatchHeader::new(28, 99999);
        assert!(header.compat_flags.is_none());

        let mut buf = Vec::new();
        header.write_to(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf);
        let restored = BatchHeader::read_from(&mut cursor).unwrap();

        assert_eq!(28, restored.protocol_version);
        assert!(restored.compat_flags.is_none());
        assert_eq!(99999, restored.checksum_seed);
    }
}
