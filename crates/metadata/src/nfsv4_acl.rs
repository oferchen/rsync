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

/// The extended attribute name for NFSv4 ACLs on Linux.
///
/// NFSv4 ACLs are stored in the `system.nfs4_acl` extended attribute rather
/// than using dedicated system calls like POSIX ACLs. This makes them portable
/// across filesystems that support extended attributes.
///
/// # Platform Notes
///
/// - **Linux**: NFSv4 ACLs are primarily supported on NFS-mounted filesystems
///   but can be used on other filesystems with extended attribute support.
/// - **Other platforms**: May use different mechanisms for NFSv4 ACL storage.
pub const NFS4_ACL_XATTR: &str = "system.nfs4_acl";

/// NFSv4 Access Control Entry (ACE) type values.
///
/// NFSv4 supports four types of ACEs that control access, auditing, and alarms.
/// In practice, most systems use only `Allow` and `Deny` types.
///
/// # Wire Format
///
/// ACE types are stored as 32-bit big-endian integers in the binary representation.
///
/// # References
///
/// - RFC 3530: NFS version 4 Protocol (Section 5.11)
/// - RFC 7530: NFSv4 Protocol (Section 6)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AceType {
    /// Access allowed.
    ///
    /// Grants the specified permissions to the principal.
    Allow = 0,
    /// Access denied.
    ///
    /// Explicitly denies the specified permissions to the principal.
    /// Deny ACEs are typically evaluated before Allow ACEs.
    Deny = 1,
    /// Audit (log access attempts).
    ///
    /// Logs access attempts matching the specified permissions.
    /// Requires audit subsystem support.
    Audit = 2,
    /// Alarm (trigger alarm on access).
    ///
    /// Triggers an alarm when access matching the specified permissions occurs.
    /// Rarely supported in practice.
    Alarm = 3,
}

impl TryFrom<u32> for AceType {
    type Error = io::Error;

    /// Converts a raw u32 value to an [`AceType`].
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] with [`io::ErrorKind::InvalidData`] if the value
    /// is not a valid ACE type (must be 0-3).
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

/// NFSv4 Access Control Entry flags.
///
/// Flags control ACE inheritance, principal type, and audit behavior.
/// The most commonly used flags are `FILE_INHERIT`, `DIRECTORY_INHERIT`,
/// and `IDENTIFIER_GROUP`.
///
/// # Wire Format
///
/// Flags are stored as a 32-bit big-endian integer with individual bits
/// representing different flag values.
///
/// # References
///
/// - RFC 3530: Section 5.11 (ACE Flags)
/// - RFC 7530: Section 6.2.1.4 (ACE4 Flags)
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

    /// Creates flags from a raw u32 value.
    ///
    /// No validation is performed; any bit pattern is accepted.
    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw flags value as a u32.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    /// Checks if a specific flag bit is set.
    ///
    /// # Examples
    ///
    /// ```
    /// # use metadata::nfsv4_acl::AceFlags;
    /// let flags = AceFlags::from_raw(AceFlags::FILE_INHERIT | AceFlags::DIRECTORY_INHERIT);
    /// assert!(flags.contains(AceFlags::FILE_INHERIT));
    /// assert!(!flags.contains(AceFlags::INHERIT_ONLY));
    /// ```
    #[must_use]
    pub const fn contains(self, flag: u32) -> bool {
        (self.0 & flag) != 0
    }
}

/// NFSv4 access mask representing permission bits.
///
/// NFSv4 provides fine-grained permissions beyond traditional Unix rwx model.
/// The access mask contains 14 permission bits covering file operations,
/// metadata access, and ACL management.
///
/// # Permission Bits
///
/// - **Data access**: `READ_DATA`, `WRITE_DATA`, `APPEND_DATA`, `EXECUTE`
/// - **Attributes**: `READ_ATTRIBUTES`, `WRITE_ATTRIBUTES`, `READ_NAMED_ATTRS`, `WRITE_NAMED_ATTRS`
/// - **ACL/Owner**: `READ_ACL`, `WRITE_ACL`, `WRITE_OWNER`
/// - **Directory**: `DELETE_CHILD` (delete files within directory)
/// - **File**: `DELETE` (delete the file itself)
///
/// # Wire Format
///
/// Stored as a 32-bit big-endian integer.
///
/// # References
///
/// - RFC 3530: Section 5.11.1 (Access Mask)
/// - RFC 7530: Section 6.2.1.3 (Access Mask)
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

    /// Creates an access mask from a raw u32 value.
    ///
    /// No validation is performed; any bit pattern is accepted.
    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw access mask value as a u32.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }
}

/// A single NFSv4 Access Control Entry (ACE).
///
/// An ACE specifies what access permissions to grant or deny for a specific
/// principal. ACEs are evaluated in order within an ACL, with the first
/// matching entry determining the access decision.
///
/// # Wire Format
///
/// Each ACE is serialized as:
/// - `ace_type`: 4 bytes (big-endian u32)
/// - `flags`: 4 bytes (big-endian u32)
/// - `mask`: 4 bytes (big-endian u32)
/// - `who_len`: 4 bytes (big-endian u32)
/// - `who`: variable length UTF-8 string
/// - padding: 0-3 bytes to align to 4-byte boundary
///
/// # Special Principals
///
/// The `who` field can contain special identifiers:
/// - `OWNER@`: The file owner
/// - `GROUP@`: The file group
/// - `EVERYONE@`: All users
/// - Or a specific user/group name
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nfs4Ace {
    /// Type of ACE (allow/deny/audit/alarm).
    pub ace_type: AceType,
    /// ACE flags (inheritance, audit behavior, etc.).
    pub flags: AceFlags,
    /// Access mask (permission bits).
    pub mask: AccessMask,
    /// Principal identifier (user/group name or special identifier like "OWNER@").
    pub who: String,
}

/// An NFSv4 Access Control List.
///
/// An ACL is an ordered list of Access Control Entries (ACEs). The order is
/// significant because NFSv4 ACLs use first-match semantics: the first ACE
/// that matches a principal's access request determines the result.
///
/// # Evaluation Order
///
/// Best practice for ACL ordering:
/// 1. Explicit deny ACEs (security-critical denials)
/// 2. Allow ACEs for specific users/groups
/// 3. Allow ACEs for broader groups
/// 4. EVERYONE@ allow ACE (if used)
///
/// # Wire Format
///
/// Serialized as a sequence of ACEs with no header or count field.
/// The ACL ends when the data is exhausted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nfs4Acl {
    /// The list of ACEs in evaluation order.
    ///
    /// Order matters: ACEs are evaluated sequentially with first-match semantics.
    pub aces: Vec<Nfs4Ace>,
}

impl Nfs4Acl {
    /// Creates an empty ACL with no ACEs.
    #[must_use]
    pub const fn new() -> Self {
        Self { aces: Vec::new() }
    }

    /// Parses an NFSv4 ACL from its binary representation.
    ///
    /// Deserializes the wire format into an [`Nfs4Acl`] structure.
    ///
    /// # Wire Format
    ///
    /// The format is a sequence of ACEs, each containing:
    /// - `type`: 4 bytes (big-endian u32)
    /// - `flags`: 4 bytes (big-endian u32)
    /// - `mask`: 4 bytes (big-endian u32)
    /// - `who_len`: 4 bytes (big-endian u32)
    /// - `who`: `who_len` bytes (UTF-8 string)
    /// - padding: 0-3 bytes to align next ACE to 4-byte boundary
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] with [`io::ErrorKind::InvalidData`] if:
    /// - The data is truncated (insufficient bytes for an ACE)
    /// - An ACE type value is invalid (not 0-3)
    /// - The `who` field contains invalid UTF-8
    ///
    /// # Examples
    ///
    /// ```
    /// # use metadata::nfsv4_acl::Nfs4Acl;
    /// let data = vec![]; // Empty ACL
    /// let acl = Nfs4Acl::from_bytes(&data).unwrap();
    /// assert!(acl.is_empty());
    /// ```
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

    /// Serializes the ACL to its binary wire format.
    ///
    /// Converts the ACL to the format expected by the `system.nfs4_acl`
    /// extended attribute.
    ///
    /// # Format
    ///
    /// Each ACE is serialized as:
    /// - 4 bytes: ACE type (big-endian)
    /// - 4 bytes: flags (big-endian)
    /// - 4 bytes: access mask (big-endian)
    /// - 4 bytes: who string length (big-endian)
    /// - N bytes: who string (UTF-8)
    /// - 0-3 bytes: padding to 4-byte alignment
    ///
    /// # Returns
    ///
    /// A byte vector containing the serialized ACL. For an empty ACL,
    /// returns an empty vector.
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

    /// Returns `true` if the ACL contains no ACEs.
    ///
    /// An empty ACL typically means the file uses only standard Unix
    /// permission bits (mode) for access control.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.aces.is_empty()
    }
}

/// Reads the NFSv4 ACL from a file.
///
/// Retrieves the NFSv4 ACL by reading the `system.nfs4_acl` extended attribute
/// and parsing it into an [`Nfs4Acl`] structure.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `follow_symlinks` - If `true`, follows symlinks and reads the target's ACL.
///   If `false`, reads the ACL of the symlink itself (though symlinks typically
///   don't have ACLs).
///
/// # Returns
///
/// - `Ok(Some(acl))` if the file has an NFSv4 ACL
/// - `Ok(None)` if the file has no NFSv4 ACL or the filesystem doesn't support them
/// - `Err(...)` for other I/O errors
///
/// # Errors
///
/// Returns [`MetadataError`] if reading or parsing the ACL fails, except for:
/// - `ENODATA`: No ACL present (returns `Ok(None)`)
/// - `ENOTSUP`/`EOPNOTSUPP`: Filesystem doesn't support ACLs (returns `Ok(None)`)
/// - `ENOENT`: File not found during attribute read (returns `Ok(None)`)
///
/// # Platform Support
///
/// Primarily supported on Linux with NFSv4-mounted filesystems or filesystems
/// that support the `system.nfs4_acl` extended attribute.
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
/// Writes the ACL to the `system.nfs4_acl` extended attribute. If the ACL
/// is `None` or empty, removes the NFSv4 ACL attribute from the file.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `acl` - The ACL to set, or `None` to remove the ACL
/// * `follow_symlinks` - If `true`, sets the ACL on the symlink target.
///   If `false`, sets the ACL on the symlink itself.
///
/// # Errors
///
/// Returns [`MetadataError`] if writing or removing the ACL fails. The error
/// is suppressed if removing a non-existent attribute (`ENODATA`/`ENOENT`).
///
/// # Platform Support
///
/// Requires filesystem support for the `system.nfs4_acl` extended attribute.
/// Commonly supported on Linux NFSv4 mounts and some local filesystems.
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
/// Copies the NFSv4 ACL from the source file to the destination, replicating
/// the exact access control configuration. If the source has no NFSv4 ACL,
/// any existing ACL on the destination is removed.
///
/// # Arguments
///
/// * `source` - Path to the source file
/// * `destination` - Path to the destination file
/// * `follow_symlinks` - If `true`, follows symlinks on both source and destination.
///   If `false`, operates on the symlinks themselves.
///
/// # Behavior
///
/// 1. Reads the NFSv4 ACL from the source file
/// 2. If source has an ACL, applies it to the destination
/// 3. If source has no ACL, removes any existing ACL from destination
///
/// This ensures the destination's NFSv4 ACL state matches the source.
///
/// # Errors
///
/// Returns [`MetadataError`] if reading the source ACL or writing to the
/// destination fails. Unsupported filesystem errors (no NFSv4 ACL support)
/// are treated as "no ACL present" and do not trigger an error.
///
/// # Platform Support
///
/// Requires both source and destination filesystems to support the
/// `system.nfs4_acl` extended attribute.
pub fn sync_nfsv4_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let acl = get_nfsv4_acl(source, follow_symlinks)?;
    set_nfsv4_acl(destination, acl.as_ref(), follow_symlinks)
}

/// Returns `true` if the path has an NFSv4 ACL.
///
/// This is a convenience function that checks whether a file has an NFSv4 ACL
/// without requiring error handling.
///
/// # Arguments
///
/// * `path` - Path to check
/// * `follow_symlinks` - Whether to follow symlinks
///
/// # Returns
///
/// - `true` if the file has a non-empty NFSv4 ACL
/// - `false` if the file has no ACL, the filesystem doesn't support ACLs,
///   or an error occurred while reading
///
/// # Note
///
/// This function suppresses all errors and returns `false` if any error occurs.
/// Use [`get_nfsv4_acl`] if you need to distinguish between errors and missing ACLs.
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
