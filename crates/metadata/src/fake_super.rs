//! crates/metadata/src/fake_super.rs
//!
//! Fake super-user mode for preserving privileged metadata without root.
//!
//! When `--fake-super` is enabled, privileged file attributes (ownership,
//! device numbers, special file types) are stored in extended attributes
//! instead of being applied directly. This allows backup/restore operations
//! without requiring root privileges.
//!
//! # Wire Format
//!
//! The `user.rsync.%stat` xattr stores metadata in the format:
//! ```text
//! <mode_octal> <uid>,<gid>[ <rdev>]
//! ```
//!
//! Examples:
//! - Regular file: `100644 1000,1000`
//! - Device file: `60660 0,6 8,0` (block device major 8, minor 0)
//! - Symlink: `120777 1000,1000`

use std::fs::Metadata;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The xattr name used to store fake-super metadata.
pub const FAKE_SUPER_XATTR: &str = "user.rsync.%stat";

/// Parsed fake-super metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeSuperStat {
    /// File mode (type + permissions).
    pub mode: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// Device number (major, minor) for special files.
    pub rdev: Option<(u32, u32)>,
}

impl FakeSuperStat {
    /// Creates a new `FakeSuperStat` from file metadata.
    pub fn from_metadata(metadata: &Metadata) -> Self {
        let mode = metadata.mode();
        let uid = metadata.uid();
        let gid = metadata.gid();

        // Extract rdev for device files
        let rdev = if is_device_file(mode) {
            let rdev = metadata.rdev();
            Some((major(rdev), minor(rdev)))
        } else {
            None
        };

        Self {
            mode,
            uid,
            gid,
            rdev,
        }
    }

    /// Encodes the stat to the wire format used in xattrs.
    ///
    /// Format: `<mode_octal> <uid>,<gid>[ <rdev_major>,<rdev_minor>]`
    pub fn encode(&self) -> String {
        let mut result = format!("{:o} {},{}", self.mode, self.uid, self.gid);

        if let Some((major, minor)) = self.rdev {
            result.push_str(&format!(" {major},{minor}"));
        }

        result
    }

    /// Decodes the stat from the wire format.
    ///
    /// # Errors
    ///
    /// Returns an error if the format is invalid.
    pub fn decode(s: &str) -> io::Result<Self> {
        let parts: Vec<&str> = s.split_whitespace().collect();

        if parts.len() < 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid fake-super format: expected at least 2 parts, got {}",
                    parts.len()
                ),
            ));
        }

        // Parse mode (octal)
        let mode = u32::from_str_radix(parts[0], 8).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid mode '{}': {}", parts[0], e),
            )
        })?;

        // Parse uid,gid
        let uid_gid: Vec<&str> = parts[1].split(',').collect();
        if uid_gid.len() != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid uid,gid format: '{}'", parts[1]),
            ));
        }

        let uid: u32 = uid_gid[0].parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid uid '{}': {}", uid_gid[0], e),
            )
        })?;

        let gid: u32 = uid_gid[1].parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid gid '{}': {}", uid_gid[1], e),
            )
        })?;

        // Parse optional rdev (major,minor)
        let rdev = if parts.len() >= 3 {
            let rdev_parts: Vec<&str> = parts[2].split(',').collect();
            if rdev_parts.len() != 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid rdev format: '{}'", parts[2]),
                ));
            }

            let major: u32 = rdev_parts[0].parse().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid rdev major '{}': {}", rdev_parts[0], e),
                )
            })?;

            let minor: u32 = rdev_parts[1].parse().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid rdev minor '{}': {}", rdev_parts[1], e),
                )
            })?;

            Some((major, minor))
        } else {
            None
        };

        Ok(Self {
            mode,
            uid,
            gid,
            rdev,
        })
    }
}

/// Stores metadata as fake-super xattr on a file.
///
/// This is called when `--fake-super` is enabled and we need to preserve
/// privileged metadata that we cannot apply directly (ownership, devices).
#[cfg(all(unix, feature = "xattr"))]
pub fn store_fake_super(path: &Path, stat: &FakeSuperStat) -> io::Result<()> {
    let value = stat.encode();
    xattr::set(path, FAKE_SUPER_XATTR, value.as_bytes())
}

/// Retrieves fake-super metadata from a file's xattr.
///
/// Returns `None` if the xattr doesn't exist.
#[cfg(all(unix, feature = "xattr"))]
pub fn load_fake_super(path: &Path) -> io::Result<Option<FakeSuperStat>> {
    match xattr::get(path, FAKE_SUPER_XATTR) {
        Ok(Some(value)) => {
            let s = String::from_utf8(value).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid UTF-8 in fake-super xattr: {e}"),
                )
            })?;
            Ok(Some(FakeSuperStat::decode(&s)?))
        }
        Ok(None) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Removes fake-super metadata from a file.
#[cfg(all(unix, feature = "xattr"))]
pub fn remove_fake_super(path: &Path) -> io::Result<()> {
    match xattr::remove(path, FAKE_SUPER_XATTR) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Checks if the file mode indicates a device file.
const fn is_device_file(mode: u32) -> bool {
    const S_IFMT: u32 = 0o170000;
    const S_IFBLK: u32 = 0o060000;
    const S_IFCHR: u32 = 0o020000;

    let file_type = mode & S_IFMT;
    file_type == S_IFBLK || file_type == S_IFCHR
}

/// Extracts the major device number from a combined rdev value.
#[cfg(target_os = "linux")]
const fn major(rdev: u64) -> u32 {
    ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32)
}

#[cfg(not(target_os = "linux"))]
fn major(rdev: u64) -> u32 {
    (rdev >> 24) as u32
}

/// Extracts the minor device number from a combined rdev value.
#[cfg(target_os = "linux")]
const fn minor(rdev: u64) -> u32 {
    (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32)
}

#[cfg(not(target_os = "linux"))]
fn minor(rdev: u64) -> u32 {
    (rdev & 0xffffff) as u32
}

/// Stores metadata as fake-super xattr on a file (stub for non-Unix/no-xattr).
///
/// Returns an error since xattr support is required.
#[cfg(not(all(unix, feature = "xattr")))]
pub fn store_fake_super(_path: &Path, _stat: &FakeSuperStat) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fake-super requires xattr support",
    ))
}

/// Retrieves fake-super metadata from a file's xattr (stub for non-Unix/no-xattr).
///
/// Always returns `None` since xattr support is not available.
#[cfg(not(all(unix, feature = "xattr")))]
pub fn load_fake_super(_path: &Path) -> io::Result<Option<FakeSuperStat>> {
    Ok(None)
}

/// Removes fake-super metadata from a file (stub for non-Unix/no-xattr).
///
/// No-op since xattr support is not available.
#[cfg(not(all(unix, feature = "xattr")))]
pub fn remove_fake_super(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_regular_file() {
        let stat = FakeSuperStat {
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            rdev: None,
        };
        assert_eq!(stat.encode(), "100644 1000,1000");
    }

    #[test]
    fn test_encode_directory() {
        let stat = FakeSuperStat {
            mode: 0o40755,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        assert_eq!(stat.encode(), "40755 0,0");
    }

    #[test]
    fn test_encode_block_device() {
        let stat = FakeSuperStat {
            mode: 0o60660,
            uid: 0,
            gid: 6,
            rdev: Some((8, 0)),
        };
        assert_eq!(stat.encode(), "60660 0,6 8,0");
    }

    #[test]
    fn test_encode_char_device() {
        let stat = FakeSuperStat {
            mode: 0o20666,
            uid: 0,
            gid: 0,
            rdev: Some((1, 3)),
        };
        assert_eq!(stat.encode(), "20666 0,0 1,3");
    }

    #[test]
    fn test_encode_symlink() {
        let stat = FakeSuperStat {
            mode: 0o120777,
            uid: 1000,
            gid: 1000,
            rdev: None,
        };
        assert_eq!(stat.encode(), "120777 1000,1000");
    }

    #[test]
    fn test_decode_regular_file() {
        let stat = FakeSuperStat::decode("100644 1000,1000").unwrap();
        assert_eq!(stat.mode, 0o100644);
        assert_eq!(stat.uid, 1000);
        assert_eq!(stat.gid, 1000);
        assert_eq!(stat.rdev, None);
    }

    #[test]
    fn test_decode_block_device() {
        let stat = FakeSuperStat::decode("60660 0,6 8,0").unwrap();
        assert_eq!(stat.mode, 0o60660);
        assert_eq!(stat.uid, 0);
        assert_eq!(stat.gid, 6);
        assert_eq!(stat.rdev, Some((8, 0)));
    }

    #[test]
    fn test_decode_roundtrip() {
        let original = FakeSuperStat {
            mode: 0o100755,
            uid: 500,
            gid: 500,
            rdev: None,
        };

        let encoded = original.encode();
        let decoded = FakeSuperStat::decode(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_decode_roundtrip_with_rdev() {
        let original = FakeSuperStat {
            mode: 0o60660,
            uid: 0,
            gid: 6,
            rdev: Some((8, 1)),
        };

        let encoded = original.encode();
        let decoded = FakeSuperStat::decode(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_decode_invalid_format() {
        assert!(FakeSuperStat::decode("").is_err());
        assert!(FakeSuperStat::decode("100644").is_err());
        assert!(FakeSuperStat::decode("invalid 1000,1000").is_err());
        assert!(FakeSuperStat::decode("100644 invalid").is_err());
        assert!(FakeSuperStat::decode("100644 1000").is_err());
    }

    #[test]
    fn test_is_device_file() {
        assert!(!is_device_file(0o100644)); // Regular file
        assert!(!is_device_file(0o40755)); // Directory
        assert!(!is_device_file(0o120777)); // Symlink
        assert!(is_device_file(0o60660)); // Block device
        assert!(is_device_file(0o20666)); // Char device
    }
}
