//! ACL wire protocol encoding and decoding functions.
//!
//! Implements the send/receive functions for ACL data exchange,
//! mirroring upstream rsync's `acls.c` implementation.

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

use super::constants::{
    ACCESS_SHIFT, NAME_IS_USER, NO_ENTRY, XFLAG_NAME_FOLLOWS, XFLAG_NAME_IS_USER, XMIT_GROUP_OBJ,
    XMIT_MASK_OBJ, XMIT_NAME_LIST, XMIT_OTHER_OBJ, XMIT_USER_OBJ,
};
use super::entry::{AclCache, IdAccess, IdaEntries, RsyncAcl};

/// ACL type for wire protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AclType {
    /// Access ACL (file permissions).
    Access,
    /// Default ACL (inherited by new files in directory).
    Default,
}

/// Result of receiving an ACL from the wire.
#[derive(Debug)]
pub enum RecvAclResult {
    /// Cache hit - use the ACL at this index.
    CacheHit(u32),
    /// Literal ACL data was received.
    Literal(RsyncAcl),
}

/// Encodes access permission bits for wire transmission.
///
/// Shifts access bits left by 2 and sets the lower 2 bits as flags.
/// This encoding keeps high bits clear for efficient varint encoding.
///
/// # Arguments
///
/// * `access` - Permission bits (rwx) with optional `NAME_IS_USER` flag
/// * `include_name` - Whether a name string will follow (sets `XFLAG_NAME_FOLLOWS`)
///
/// # Upstream Reference
///
/// See `acls.c` lines 48-53 for the encoding rationale.
fn encode_access(access: u32, include_name: bool) -> u32 {
    let perms = access & !NAME_IS_USER;
    let mut encoded = perms << ACCESS_SHIFT;

    if include_name {
        encoded |= XFLAG_NAME_FOLLOWS;
    }
    if access & NAME_IS_USER != 0 {
        encoded |= XFLAG_NAME_IS_USER;
    }

    encoded
}

/// Decodes access permission bits from wire format.
///
/// Extracts the flags from lower 2 bits and shifts to get permission bits.
///
/// # Returns
///
/// Tuple of (access_with_flags, name_follows) where access_with_flags
/// has `NAME_IS_USER` set if the entry is for a user.
///
/// # Upstream Reference
///
/// Mirrors `recv_acl_access()` in `acls.c` lines 672-695.
fn decode_access(encoded: u32, is_name_entry: bool) -> (u32, bool) {
    if is_name_entry {
        let flags = encoded & 0x03;
        let mut access = encoded >> ACCESS_SHIFT;

        let name_follows = flags & XFLAG_NAME_FOLLOWS != 0;
        if flags & XFLAG_NAME_IS_USER != 0 {
            access |= NAME_IS_USER;
        }

        (access, name_follows)
    } else {
        (encoded, false)
    }
}

/// Sends the ida_entries list over the wire.
///
/// # Wire Format
///
/// ```text
/// count      : varint
/// For each entry:
///   id       : varint
///   access   : varint  // (perms << 2) | flags
///   [len]    : byte    // if XFLAG_NAME_FOLLOWS
///   [name]   : bytes   // if XFLAG_NAME_FOLLOWS
/// ```
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `entries` - The named user/group entries to send
/// * `include_names` - Whether to include user/group name strings
///
/// # Upstream Reference
///
/// Mirrors `send_ida_entries()` in `acls.c` lines 581-605.
pub fn send_ida_entries<W: Write>(
    writer: &mut W,
    entries: &IdaEntries,
    include_names: bool,
) -> io::Result<()> {
    write_varint(writer, entries.len() as i32)?;

    for entry in entries.iter() {
        write_varint(writer, entry.id as i32)?;

        let encoded = encode_access(entry.access, include_names);
        write_varint(writer, encoded as i32)?;

        // Name transmission would go here if include_names is true
        // For now, we don't include names (numeric_ids mode)
        // This matches upstream behavior when numeric_ids is set
    }

    Ok(())
}

/// Receives the ida_entries list from the wire.
///
/// # Returns
///
/// The decoded entries and the computed mask bits (OR of all access values).
///
/// # Upstream Reference
///
/// Mirrors `recv_ida_entries()` in `acls.c` lines 697-729.
pub fn recv_ida_entries<R: Read>(reader: &mut R) -> io::Result<(IdaEntries, u8)> {
    let count = read_varint(reader)? as usize;
    let mut entries = IdaEntries::with_capacity(count);
    let mut computed_mask: u8 = 0;

    for _ in 0..count {
        let id = read_varint(reader)? as u32;
        let encoded = read_varint(reader)? as u32;

        let (access, name_follows) = decode_access(encoded, true);

        if name_follows {
            // Read and discard name for now
            // In full implementation, this would do UID/GID resolution
            let mut len_buf = [0u8; 1];
            reader.read_exact(&mut len_buf)?;
            let name_len = len_buf[0] as usize;
            let mut name_buf = vec![0u8; name_len];
            reader.read_exact(&mut name_buf)?;
        }

        entries.push(IdAccess { id, access });
        computed_mask |= (access & !NAME_IS_USER) as u8;
    }

    Ok((entries, computed_mask & !NO_ENTRY))
}

/// Sends an rsync ACL to the wire.
///
/// If the ACL matches a previously sent one (found in cache), only the
/// index is sent. Otherwise, the full ACL is encoded and added to cache.
///
/// # Wire Format
///
/// ```text
/// ndx + 1    : varint  // 0 means literal follows, >0 is cache index + 1
/// If ndx == 0 (literal):
///   flags    : byte    // XMIT_* flags
///   [user_obj]   : varint if XMIT_USER_OBJ
///   [group_obj]  : varint if XMIT_GROUP_OBJ
///   [mask_obj]   : varint if XMIT_MASK_OBJ
///   [other_obj]  : varint if XMIT_OTHER_OBJ
///   [names]      : ida_entries if XMIT_NAME_LIST
/// ```
///
/// # Upstream Reference
///
/// Mirrors `send_rsync_acl()` in `acls.c` lines 607-647.
pub fn send_rsync_acl<W: Write>(
    writer: &mut W,
    acl: &RsyncAcl,
    acl_type: AclType,
    cache: &mut AclCache,
    include_names: bool,
) -> io::Result<()> {
    // Check cache for matching ACL
    let cached_index = match acl_type {
        AclType::Access => cache.find_access(acl),
        AclType::Default => cache.find_default(acl),
    };

    // Send index + 1 (0 means literal data follows)
    let ndx = cached_index.map(|i| i as i32).unwrap_or(-1);
    write_varint(writer, ndx + 1)?;

    if cached_index.is_some() {
        return Ok(());
    }

    // Store in cache for future matches
    let acl_clone = acl.clone();
    match acl_type {
        AclType::Access => cache.store_access(acl_clone),
        AclType::Default => cache.store_default(acl_clone),
    };

    // Send literal ACL data
    let flags = acl.flags();
    writer.write_all(&[flags])?;

    if flags & XMIT_USER_OBJ != 0 {
        write_varint(writer, i32::from(acl.user_obj))?;
    }
    if flags & XMIT_GROUP_OBJ != 0 {
        write_varint(writer, i32::from(acl.group_obj))?;
    }
    if flags & XMIT_MASK_OBJ != 0 {
        write_varint(writer, i32::from(acl.mask_obj))?;
    }
    if flags & XMIT_OTHER_OBJ != 0 {
        write_varint(writer, i32::from(acl.other_obj))?;
    }
    if flags & XMIT_NAME_LIST != 0 {
        send_ida_entries(writer, &acl.names, include_names)?;
    }

    Ok(())
}

/// Receives an rsync ACL from the wire.
///
/// # Returns
///
/// Either a cache index (if the sender referenced a cached ACL) or
/// the literal ACL data.
///
/// # Upstream Reference
///
/// Mirrors `recv_rsync_acl()` in `acls.c` lines 731-800.
pub fn recv_rsync_acl<R: Read>(reader: &mut R) -> io::Result<RecvAclResult> {
    let ndx_plus_one = read_varint(reader)?;
    let ndx = ndx_plus_one - 1;

    if ndx >= 0 {
        return Ok(RecvAclResult::CacheHit(ndx as u32));
    }

    // Read literal ACL
    let mut flags_buf = [0u8; 1];
    reader.read_exact(&mut flags_buf)?;
    let flags = flags_buf[0];

    let mut acl = RsyncAcl::new();

    if flags & XMIT_USER_OBJ != 0 {
        acl.user_obj = read_varint(reader)? as u8;
    }
    if flags & XMIT_GROUP_OBJ != 0 {
        acl.group_obj = read_varint(reader)? as u8;
    }
    if flags & XMIT_MASK_OBJ != 0 {
        acl.mask_obj = read_varint(reader)? as u8;
    }
    if flags & XMIT_OTHER_OBJ != 0 {
        acl.other_obj = read_varint(reader)? as u8;
    }
    if flags & XMIT_NAME_LIST != 0 {
        let (entries, _computed_mask) = recv_ida_entries(reader)?;
        acl.names = entries;
    }

    Ok(RecvAclResult::Literal(acl))
}

/// Sends ACL data for a file entry.
///
/// Sends the access ACL, and for directories also sends the default ACL.
///
/// # Arguments
///
/// * `writer` - Output stream
/// * `access_acl` - The file's access ACL
/// * `default_acl` - The directory's default ACL (ignored for non-directories)
/// * `is_directory` - Whether this entry is a directory
/// * `cache` - ACL cache for deduplication
///
/// # Upstream Reference
///
/// Mirrors `send_acl()` in `acls.c` lines 651-668.
pub fn send_acl<W: Write>(
    writer: &mut W,
    access_acl: &RsyncAcl,
    default_acl: Option<&RsyncAcl>,
    is_directory: bool,
    cache: &mut AclCache,
) -> io::Result<()> {
    send_rsync_acl(writer, access_acl, AclType::Access, cache, false)?;

    if is_directory {
        let def_acl = default_acl.cloned().unwrap_or_default();
        send_rsync_acl(writer, &def_acl, AclType::Default, cache, false)?;
    }

    Ok(())
}

/// Receives ACL data for a file entry.
///
/// Receives the access ACL, and for directories also receives the default ACL.
///
/// # Returns
///
/// Tuple of (access_result, optional_default_result).
///
/// # Upstream Reference
///
/// Mirrors `receive_acl()` in `acls.c` (implicit in the flist receive path).
pub fn recv_acl<R: Read>(
    reader: &mut R,
    is_directory: bool,
) -> io::Result<(RecvAclResult, Option<RecvAclResult>)> {
    let access_result = recv_rsync_acl(reader)?;

    let default_result = if is_directory {
        Some(recv_rsync_acl(reader)?)
    } else {
        None
    };

    Ok((access_result, default_result))
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encode_decode_access_roundtrip() {
        // User entry with rwx
        let access = 0x07 | NAME_IS_USER;
        let encoded = encode_access(access, false);
        let (decoded, name_follows) = decode_access(encoded, true);
        assert_eq!(decoded & !NAME_IS_USER, access & !NAME_IS_USER);
        assert!(decoded & NAME_IS_USER != 0);
        assert!(!name_follows);

        // Group entry with rx
        let access = 0x05;
        let encoded = encode_access(access, true);
        let (decoded, name_follows) = decode_access(encoded, true);
        assert_eq!(decoded, access);
        assert!(name_follows);
    }

    #[test]
    fn send_recv_empty_acl() {
        let acl = RsyncAcl::new();
        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_rsync_acl(&mut cursor).unwrap();

        match result {
            RecvAclResult::Literal(received) => {
                assert!(received.is_empty());
            }
            RecvAclResult::CacheHit(_) => panic!("Expected literal ACL"),
        }
    }

    #[test]
    fn send_recv_acl_with_entries() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07; // rwx
        acl.group_obj = 0x05; // r-x
        acl.other_obj = 0x04; // r--

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_rsync_acl(&mut cursor).unwrap();

        match result {
            RecvAclResult::Literal(received) => {
                assert_eq!(received.user_obj, 0x07);
                assert_eq!(received.group_obj, 0x05);
                assert_eq!(received.other_obj, 0x04);
                assert_eq!(received.mask_obj, NO_ENTRY);
            }
            RecvAclResult::CacheHit(_) => panic!("Expected literal ACL"),
        }
    }

    #[test]
    fn cache_hit_on_second_send() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        // First send - should be literal
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();
        let first_len = buf.len();

        // Second send of same ACL - should be cache hit (shorter)
        buf.clear();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        assert!(buf.len() < first_len, "Cache hit should be shorter");

        let mut cursor = Cursor::new(buf);
        let result = recv_rsync_acl(&mut cursor).unwrap();

        match result {
            RecvAclResult::CacheHit(idx) => {
                assert_eq!(idx, 0);
            }
            RecvAclResult::Literal(_) => panic!("Expected cache hit"),
        }
    }

    #[test]
    fn send_recv_ida_entries_roundtrip() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x07));
        entries.push(IdAccess::group(100, 0x05));

        let mut buf = Vec::new();
        send_ida_entries(&mut buf, &entries, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let (received, mask) = recv_ida_entries(&mut cursor).unwrap();

        assert_eq!(received.len(), 2);
        assert_eq!(mask, 0x07); // OR of all permissions
    }

    #[test]
    fn send_recv_directory_acl() {
        let access_acl = {
            let mut acl = RsyncAcl::new();
            acl.user_obj = 0x07;
            acl.group_obj = 0x05;
            acl.other_obj = 0x05;
            acl
        };

        let default_acl = {
            let mut acl = RsyncAcl::new();
            acl.user_obj = 0x07;
            acl.group_obj = 0x05;
            acl.other_obj = 0x00;
            acl
        };

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_acl(&mut buf, &access_acl, Some(&default_acl), true, &mut cache).unwrap();

        let mut cursor = Cursor::new(buf);
        let (access_result, default_result) = recv_acl(&mut cursor, true).unwrap();

        assert!(matches!(access_result, RecvAclResult::Literal(_)));
        assert!(default_result.is_some());
        assert!(matches!(default_result.unwrap(), RecvAclResult::Literal(_)));
    }
}
