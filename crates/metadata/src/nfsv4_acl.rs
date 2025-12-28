//! crates/metadata/src/nfsv4_acl.rs
//!
//! NFSv4 Access Control List support for rsync transfers.
//!
//! NFSv4 ACLs differ significantly from POSIX ACLs:
//!
//! - **ACE-based model**: Each Access Control Entry (ACE) specifies allow/deny
//!   permissions for a specific principal (user, group, or special identifiers).
//! - **Granular permissions**: 14 distinct permission bits vs POSIX's 3 (rwx).
//! - **Inheritance**: Rich inheritance model for directories.
//! - **Order matters**: ACEs are evaluated in order; first match wins.
//!
//! On Linux, NFSv4 ACLs are stored in the `system.nfs4_acl` extended attribute.
//! This module provides synchronization of NFSv4 ACLs between files.
//!
//! # Wire Format
//!
//! NFSv4 ACLs are stored as a sequence of ACEs. Each ACE contains:
//! - Type (4 bytes): ALLOW (0) or DENY (1)
//! - Flags (4 bytes): Inheritance and audit flags
//! - Mask (4 bytes): Permission bits
//! - Who (variable): Principal identifier string
//!
//! # Examples
//!
//! ```rust,ignore
//! use metadata::nfsv4_acl::sync_nfsv4_acls;
//! use std::path::Path;
//!
//! sync_nfsv4_acls(
//!     Path::new("/source/file"),
//!     Path::new("/dest/file"),
//!     false, // don't follow symlinks
//! )?;
//! ```

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use crate::MetadataError;

/// The extended attribute name for NFSv4 ACLs.
pub const NFS4_ACL_XATTR: &str = "system.nfs4_acl";

/// NFSv4 ACE type values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AceType {
    /// Access allowed.
    Allow = 0,
    /// Access denied.
    Deny = 1,
    /// Audit (log access attempts).
    Audit = 2,
    /// Alarm (trigger alarm on access).
    Alarm = 3,
}

impl TryFrom<u32> for AceType {
    type Error = io::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Allow),
            1 => Ok(Self::Deny),
            2 => Ok(Self::Audit),
            3 => Ok(Self::Alarm),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid NFSv4 ACE type: {value}"),
            )),
        }
    }
}

/// NFSv4 ACE flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AceFlags(u32);

impl AceFlags {
    /// ACE applies to files in this directory.
    pub const FILE_INHERIT: u32 = 0x0001;
    /// ACE applies to subdirectories.
    pub const DIRECTORY_INHERIT: u32 = 0x0002;
    /// Don't propagate inheritance to children of children.
    pub const NO_PROPAGATE_INHERIT: u32 = 0x0004;
    /// ACE is for inheritance only, doesn't apply to this object.
    pub const INHERIT_ONLY: u32 = 0x0008;
    /// Audit successful accesses.
    pub const SUCCESSFUL_ACCESS: u32 = 0x0010;
    /// Audit failed accesses.
    pub const FAILED_ACCESS: u32 = 0x0020;
    /// Principal is a group.
    pub const IDENTIFIER_GROUP: u32 = 0x0040;
    /// ACE was inherited from parent.
    pub const INHERITED: u32 = 0x0080;

    /// Creates flags from raw value.
    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw flags value.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    /// Checks if a flag is set.
    #[must_use]
    pub const fn contains(self, flag: u32) -> bool {
        (self.0 & flag) != 0
    }
}

/// NFSv4 access mask (permission bits).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccessMask(u32);

impl AccessMask {
    /// Read data from file / list directory.
    pub const READ_DATA: u32 = 0x0001;
    /// Write data to file / create file in directory.
    pub const WRITE_DATA: u32 = 0x0002;
    /// Append data to file / create subdirectory.
    pub const APPEND_DATA: u32 = 0x0004;
    /// Read named attributes.
    pub const READ_NAMED_ATTRS: u32 = 0x0008;
    /// Write named attributes.
    pub const WRITE_NAMED_ATTRS: u32 = 0x0010;
    /// Execute file / search directory.
    pub const EXECUTE: u32 = 0x0020;
    /// Delete a file within a directory.
    pub const DELETE_CHILD: u32 = 0x0040;
    /// Read file attributes.
    pub const READ_ATTRIBUTES: u32 = 0x0080;
    /// Write file attributes.
    pub const WRITE_ATTRIBUTES: u32 = 0x0100;
    /// Delete the file itself.
    pub const DELETE: u32 = 0x10000;
    /// Read the ACL.
    pub const READ_ACL: u32 = 0x20000;
    /// Write the ACL.
    pub const WRITE_ACL: u32 = 0x40000;
    /// Change owner.
    pub const WRITE_OWNER: u32 = 0x80000;
    /// Synchronize (Windows semantics).
    pub const SYNCHRONIZE: u32 = 0x100000;

    /// Creates a mask from raw value.
    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw mask value.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }
}

/// A single NFSv4 Access Control Entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nfs4Ace {
    /// Type of ACE (allow/deny/audit/alarm).
    pub ace_type: AceType,
    /// ACE flags (inheritance, etc.).
    pub flags: AceFlags,
    /// Access mask (permissions).
    pub mask: AccessMask,
    /// Principal identifier (user/group name or special identifier).
    pub who: String,
}

/// An NFSv4 Access Control List.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nfs4Acl {
    /// The list of ACEs in evaluation order.
    pub aces: Vec<Nfs4Ace>,
}

impl Nfs4Acl {
    /// Creates an empty ACL.
    #[must_use]
    pub const fn new() -> Self {
        Self { aces: Vec::new() }
    }

    /// Parses an NFSv4 ACL from its binary representation.
    ///
    /// The format is a sequence of ACEs, each containing:
    /// - type (4 bytes, big-endian)
    /// - flags (4 bytes, big-endian)
    /// - mask (4 bytes, big-endian)
    /// - who_len (4 bytes, big-endian)
    /// - who (who_len bytes, UTF-8 string)
    pub fn from_bytes(data: &[u8]) -> io::Result<Self> {
        let mut aces = Vec::new();
        let mut offset = 0;

        while offset + 16 <= data.len() {
            // Read ACE header (16 bytes minimum)
            let ace_type = u32::from_be_bytes(
                data[offset..offset + 4]
                    .try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "truncated ACE"))?,
            );
            offset += 4;

            let flags = u32::from_be_bytes(
                data[offset..offset + 4]
                    .try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "truncated ACE"))?,
            );
            offset += 4;

            let mask = u32::from_be_bytes(
                data[offset..offset + 4]
                    .try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "truncated ACE"))?,
            );
            offset += 4;

            let who_len = u32::from_be_bytes(
                data[offset..offset + 4]
                    .try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "truncated ACE"))?,
            ) as usize;
            offset += 4;

            // Read who string
            if offset + who_len > data.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated ACE who field",
                ));
            }

            let who = std::str::from_utf8(&data[offset..offset + who_len])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 in ACE"))?
                .to_owned();
            offset += who_len;

            // Align to 4 bytes
            let padding = (4 - (who_len % 4)) % 4;
            offset += padding;

            aces.push(Nfs4Ace {
                ace_type: AceType::try_from(ace_type)?,
                flags: AceFlags::from_raw(flags),
                mask: AccessMask::from_raw(mask),
                who,
            });
        }

        Ok(Self { aces })
    }

    /// Serializes the ACL to its binary representation.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();

        for ace in &self.aces {
            // Write ACE header
            data.extend_from_slice(&(ace.ace_type as u32).to_be_bytes());
            data.extend_from_slice(&ace.flags.as_raw().to_be_bytes());
            data.extend_from_slice(&ace.mask.as_raw().to_be_bytes());

            let who_bytes = ace.who.as_bytes();
            data.extend_from_slice(&(who_bytes.len() as u32).to_be_bytes());
            data.extend_from_slice(who_bytes);

            // Align to 4 bytes
            let padding = (4 - (who_bytes.len() % 4)) % 4;
            data.extend(std::iter::repeat_n(0u8, padding));
        }

        data
    }

    /// Returns true if the ACL is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.aces.is_empty()
    }
}

/// Reads the NFSv4 ACL from a file.
///
/// Returns `None` if the file has no NFSv4 ACL or the filesystem doesn't
/// support NFSv4 ACLs.
pub fn get_nfsv4_acl(path: &Path, follow_symlinks: bool) -> Result<Option<Nfs4Acl>, MetadataError> {
    let name = OsStr::new(NFS4_ACL_XATTR);

    let result = if follow_symlinks {
        xattr::get_deref(path, name)
    } else {
        xattr::get(path, name)
    };

    match result {
        Ok(Some(data)) => {
            let acl = Nfs4Acl::from_bytes(&data)
                .map_err(|e| MetadataError::new("parse NFSv4 ACL", path, e))?;
            Ok(Some(acl))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            // ENODATA, ENOTSUP, EOPNOTSUPP mean no ACL or unsupported
            let kind = e.kind();
            if kind == io::ErrorKind::NotFound
                || kind == io::ErrorKind::Unsupported
                || e.raw_os_error() == Some(libc::ENODATA)
                || e.raw_os_error() == Some(libc::EOPNOTSUPP)
            {
                Ok(None)
            } else {
                Err(MetadataError::new("read NFSv4 ACL", path, e))
            }
        }
    }
}

/// Sets the NFSv4 ACL on a file.
///
/// If `acl` is `None`, removes the NFSv4 ACL from the file.
pub fn set_nfsv4_acl(
    path: &Path,
    acl: Option<&Nfs4Acl>,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let name = OsStr::new(NFS4_ACL_XATTR);

    match acl {
        Some(acl) if !acl.is_empty() => {
            let data = acl.to_bytes();
            let result = if follow_symlinks {
                xattr::set_deref(path, name, &data)
            } else {
                xattr::set(path, name, &data)
            };
            result.map_err(|e| MetadataError::new("write NFSv4 ACL", path, e))
        }
        _ => {
            // Remove the ACL
            let result = if follow_symlinks {
                xattr::remove_deref(path, name)
            } else {
                xattr::remove(path, name)
            };
            // Ignore ENODATA (attribute doesn't exist)
            match result {
                Ok(()) => Ok(()),
                Err(e) if e.raw_os_error() == Some(libc::ENODATA) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(MetadataError::new("remove NFSv4 ACL", path, e)),
            }
        }
    }
}

/// Synchronizes NFSv4 ACLs from source to destination.
///
/// This function:
/// - Reads the NFSv4 ACL from the source file
/// - Applies it to the destination file
/// - If the source has no NFSv4 ACL, removes any existing ACL from destination
///
/// # Errors
///
/// Returns an error if reading or writing the ACL fails, except for
/// unsupported filesystem errors which are silently ignored.
pub fn sync_nfsv4_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let acl = get_nfsv4_acl(source, follow_symlinks)?;
    set_nfsv4_acl(destination, acl.as_ref(), follow_symlinks)
}

/// Returns true if the path has an NFSv4 ACL.
pub fn has_nfsv4_acl(path: &Path, follow_symlinks: bool) -> bool {
    get_nfsv4_acl(path, follow_symlinks)
        .map(|acl| acl.is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_acl_serialization() {
        let acl = Nfs4Acl::new();
        assert!(acl.is_empty());

        let bytes = acl.to_bytes();
        assert!(bytes.is_empty());

        let parsed = Nfs4Acl::from_bytes(&bytes).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn ace_roundtrip() {
        let acl = Nfs4Acl {
            aces: vec![
                Nfs4Ace {
                    ace_type: AceType::Allow,
                    flags: AceFlags::from_raw(0),
                    mask: AccessMask::from_raw(AccessMask::READ_DATA | AccessMask::EXECUTE),
                    who: "OWNER@".to_owned(),
                },
                Nfs4Ace {
                    ace_type: AceType::Deny,
                    flags: AceFlags::from_raw(AceFlags::IDENTIFIER_GROUP),
                    mask: AccessMask::from_raw(AccessMask::WRITE_DATA),
                    who: "GROUP@".to_owned(),
                },
            ],
        };

        let bytes = acl.to_bytes();
        let parsed = Nfs4Acl::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.aces.len(), 2);
        assert_eq!(parsed.aces[0].ace_type, AceType::Allow);
        assert_eq!(parsed.aces[0].who, "OWNER@");
        assert_eq!(parsed.aces[1].ace_type, AceType::Deny);
        assert_eq!(parsed.aces[1].who, "GROUP@");
    }

    #[test]
    fn ace_type_conversion() {
        assert_eq!(AceType::try_from(0).unwrap(), AceType::Allow);
        assert_eq!(AceType::try_from(1).unwrap(), AceType::Deny);
        assert_eq!(AceType::try_from(2).unwrap(), AceType::Audit);
        assert_eq!(AceType::try_from(3).unwrap(), AceType::Alarm);
        assert!(AceType::try_from(4).is_err());
    }

    #[test]
    fn flags_operations() {
        let flags = AceFlags::from_raw(AceFlags::FILE_INHERIT | AceFlags::DIRECTORY_INHERIT);
        assert!(flags.contains(AceFlags::FILE_INHERIT));
        assert!(flags.contains(AceFlags::DIRECTORY_INHERIT));
        assert!(!flags.contains(AceFlags::INHERIT_ONLY));
    }

    #[test]
    fn who_with_padding() {
        // Test a who string that requires padding (length not multiple of 4)
        let acl = Nfs4Acl {
            aces: vec![Nfs4Ace {
                ace_type: AceType::Allow,
                flags: AceFlags::default(),
                mask: AccessMask::from_raw(AccessMask::READ_DATA),
                who: "user".to_owned(), // 4 bytes, no padding needed
            }],
        };

        let bytes = acl.to_bytes();
        let parsed = Nfs4Acl::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.aces[0].who, "user");

        // Test with odd-length string
        let acl2 = Nfs4Acl {
            aces: vec![Nfs4Ace {
                ace_type: AceType::Allow,
                flags: AceFlags::default(),
                mask: AccessMask::from_raw(AccessMask::READ_DATA),
                who: "u".to_owned(), // 1 byte, needs 3 bytes padding
            }],
        };

        let bytes2 = acl2.to_bytes();
        let parsed2 = Nfs4Acl::from_bytes(&bytes2).unwrap();
        assert_eq!(parsed2.aces[0].who, "u");
    }
}
