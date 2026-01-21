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

    // =========================================================================
    // Encoding Edge Cases
    // =========================================================================

    #[test]
    fn encode_access_all_permission_bits() {
        // Test all permission combinations
        for perms in 0..=7 {
            let encoded = encode_access(perms, false);
            let (decoded, _) = decode_access(encoded, true);
            assert_eq!(
                decoded & !NAME_IS_USER,
                perms,
                "Perms 0x{perms:02X} roundtrip failed"
            );
        }
    }

    #[test]
    fn encode_access_name_is_user_flag() {
        let access = 0x05 | NAME_IS_USER;
        let encoded = encode_access(access, true);

        // Encoded value should have both flags set
        assert!(encoded & XFLAG_NAME_FOLLOWS != 0);
        assert!(encoded & XFLAG_NAME_IS_USER != 0);

        let (decoded, name_follows) = decode_access(encoded, true);
        assert!(name_follows);
        assert!(decoded & NAME_IS_USER != 0);
        assert_eq!(decoded & !NAME_IS_USER, 0x05);
    }

    #[test]
    fn decode_access_non_name_entry() {
        // Non-name entries return the raw value without flag interpretation
        let value = 0x1234;
        let (decoded, name_follows) = decode_access(value, false);
        assert_eq!(decoded, value);
        assert!(!name_follows);
    }

    #[test]
    fn encode_access_shifts_correctly() {
        // Verify ACCESS_SHIFT is applied correctly
        let perms = 0x07; // rwx
        let encoded = encode_access(perms, false);

        // Perms should be shifted left by ACCESS_SHIFT (2)
        assert_eq!(encoded >> ACCESS_SHIFT, perms);
        // Lower 2 bits should be clear (no flags)
        assert_eq!(encoded & 0x03, 0);
    }

    // =========================================================================
    // Error Path Tests - EOF Conditions
    // =========================================================================

    #[test]
    fn recv_ida_entries_eof_reading_count() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let result = recv_ida_entries(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_ida_entries_eof_reading_id() {
        // Count says 1 entry but no id follows
        let data = vec![0x01]; // count = 1
        let mut cursor = Cursor::new(data);
        let result = recv_ida_entries(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_ida_entries_eof_reading_access() {
        // Count says 1 entry, id present, but no access
        let data = vec![0x01, 0x64]; // count = 1, id = 100
        let mut cursor = Cursor::new(data);
        let result = recv_ida_entries(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_rsync_acl_eof_reading_ndx() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let result = recv_rsync_acl(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_rsync_acl_eof_reading_flags() {
        // ndx = 0 (literal) but no flags byte
        let data = vec![0x00]; // ndx + 1 = 0, so ndx = -1
        let mut cursor = Cursor::new(data);
        let result = recv_rsync_acl(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_rsync_acl_eof_reading_user_obj() {
        // ndx = 0 (literal), flags indicate user_obj, but no data
        let data = vec![0x00, XMIT_USER_OBJ]; // ndx = -1, flags = XMIT_USER_OBJ
        let mut cursor = Cursor::new(data);
        let result = recv_rsync_acl(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_rsync_acl_eof_reading_group_obj() {
        // flags indicate group_obj, but no data after user_obj
        let data = vec![0x00, XMIT_USER_OBJ | XMIT_GROUP_OBJ, 0x07]; // user_obj = 7
        let mut cursor = Cursor::new(data);
        let result = recv_rsync_acl(&mut cursor);
        assert!(result.is_err());
    }

    // =========================================================================
    // RecvAclResult Tests
    // =========================================================================

    #[test]
    fn recv_acl_result_debug_format() {
        let cache_hit = RecvAclResult::CacheHit(42);
        let debug = format!("{cache_hit:?}");
        assert!(debug.contains("CacheHit"));
        assert!(debug.contains("42"));

        let literal = RecvAclResult::Literal(RsyncAcl::new());
        let debug = format!("{literal:?}");
        assert!(debug.contains("Literal"));
    }

    #[test]
    fn acl_type_equality_and_copy() {
        let a = AclType::Access;
        let b = AclType::Access;
        let c = AclType::Default;

        assert_eq!(a, b);
        assert_ne!(a, c);

        // Test Clone/Copy
        let d = a;
        assert_eq!(a, d);
    }

    #[test]
    fn acl_type_debug_format() {
        let access = AclType::Access;
        let default = AclType::Default;

        assert!(format!("{access:?}").contains("Access"));
        assert!(format!("{default:?}").contains("Default"));
    }

    // =========================================================================
    // Name Following Tests
    // =========================================================================

    #[test]
    fn recv_ida_entries_with_name_follows() {
        use crate::varint::write_varint;

        let mut data = Vec::new();
        // count = 1
        write_varint(&mut data, 1).unwrap();
        // id = 1000
        write_varint(&mut data, 1000).unwrap();
        // access with XFLAG_NAME_FOLLOWS set: perms=7, flags=1 (name follows)
        let encoded = (0x07 << ACCESS_SHIFT) | XFLAG_NAME_FOLLOWS;
        write_varint(&mut data, encoded as i32).unwrap();
        // name length = 4
        data.push(4);
        // name = "test"
        data.extend_from_slice(b"test");

        let mut cursor = Cursor::new(data);
        let (entries, mask) = recv_ida_entries(&mut cursor).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries.iter().next().unwrap().id, 1000);
        assert_eq!(mask, 0x07);
    }

    #[test]
    fn recv_ida_entries_name_follows_eof_in_length() {
        use crate::varint::write_varint;

        let mut data = Vec::new();
        write_varint(&mut data, 1).unwrap(); // count
        write_varint(&mut data, 1000).unwrap(); // id
        let encoded = (0x07 << ACCESS_SHIFT) | XFLAG_NAME_FOLLOWS;
        write_varint(&mut data, encoded as i32).unwrap();
        // Missing name length byte

        let mut cursor = Cursor::new(data);
        let result = recv_ida_entries(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_ida_entries_name_follows_eof_in_name() {
        use crate::varint::write_varint;

        let mut data = Vec::new();
        write_varint(&mut data, 1).unwrap(); // count
        write_varint(&mut data, 1000).unwrap(); // id
        let encoded = (0x07 << ACCESS_SHIFT) | XFLAG_NAME_FOLLOWS;
        write_varint(&mut data, encoded as i32).unwrap();
        data.push(10); // name length = 10
        data.extend_from_slice(b"abc"); // Only 3 bytes instead of 10

        let mut cursor = Cursor::new(data);
        let result = recv_ida_entries(&mut cursor);
        assert!(result.is_err());
    }

    // =========================================================================
    // Cache Behavior Tests
    // =========================================================================

    #[test]
    fn separate_caches_for_access_and_default() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;

        let mut cache = AclCache::new();

        // Send as access ACL first
        let mut buf1 = Vec::new();
        send_rsync_acl(&mut buf1, &acl, AclType::Access, &mut cache, false).unwrap();

        // Send same ACL as default - should NOT hit cache (different type)
        let mut buf2 = Vec::new();
        send_rsync_acl(&mut buf2, &acl, AclType::Default, &mut cache, false).unwrap();

        // Both should be full literals (not cache hits)
        let mut cursor1 = Cursor::new(buf1);
        let result1 = recv_rsync_acl(&mut cursor1).unwrap();
        assert!(matches!(result1, RecvAclResult::Literal(_)));

        let mut cursor2 = Cursor::new(buf2);
        let result2 = recv_rsync_acl(&mut cursor2).unwrap();
        assert!(matches!(result2, RecvAclResult::Literal(_)));
    }

    #[test]
    fn send_recv_file_acl_no_default() {
        let access_acl = {
            let mut acl = RsyncAcl::new();
            acl.user_obj = 0x06;
            acl.group_obj = 0x04;
            acl.other_obj = 0x04;
            acl
        };

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        // File (not directory) - no default ACL sent
        send_acl(&mut buf, &access_acl, None, false, &mut cache).unwrap();

        let mut cursor = Cursor::new(buf);
        let (access_result, default_result) = recv_acl(&mut cursor, false).unwrap();

        assert!(matches!(access_result, RecvAclResult::Literal(_)));
        assert!(default_result.is_none());
    }

    #[test]
    fn send_recv_acl_with_mask_obj() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x07;
        acl.mask_obj = 0x05; // Effective permissions masked to r-x
        acl.other_obj = 0x04;

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_rsync_acl(&mut cursor).unwrap();

        match result {
            RecvAclResult::Literal(received) => {
                assert_eq!(received.user_obj, 0x07);
                assert_eq!(received.group_obj, 0x07);
                assert_eq!(received.mask_obj, 0x05);
                assert_eq!(received.other_obj, 0x04);
            }
            RecvAclResult::CacheHit(_) => panic!("Expected literal ACL"),
        }
    }
}
