//! ACL wire protocol receiving functions.
//!
//! Implements the receive side of the ACL wire protocol, mirroring
//! upstream rsync's `acls.c` receive functions.

use std::io::{self, Read};

use crate::varint::read_varint;

use super::super::constants::{
    NAME_IS_USER, NO_ENTRY, XMIT_GROUP_OBJ, XMIT_MASK_OBJ, XMIT_NAME_LIST, XMIT_OTHER_OBJ,
    XMIT_USER_OBJ,
};
use super::super::entry::{AclCache, IdAccess, IdaEntries, RsyncAcl};
use super::encoding::decode_access;
use super::types::{AclType, RecvAclResult};

/// Receives the ida_entries list from the wire.
///
/// # Returns
///
/// The decoded entries and the computed mask bits (OR of all access values).
///
/// # Upstream Reference
///
/// Mirrors `recv_ida_entries()` in `acls.c` lines 697-729.
pub fn recv_ida_entries<R: Read + ?Sized>(reader: &mut R) -> io::Result<(IdaEntries, u8)> {
    let count = read_varint(reader)? as usize;
    let mut entries = IdaEntries::with_capacity(count);
    let mut computed_mask: u8 = 0;

    for _ in 0..count {
        let id = read_varint(reader)? as u32;
        let encoded = read_varint(reader)? as u32;

        let (access, name_follows) = decode_access(encoded, true);

        // upstream: acls.c recv_ida_entries() reads name bytes after access
        let name = if name_follows {
            let mut len_buf = [0u8; 1];
            reader.read_exact(&mut len_buf)?;
            let name_len = len_buf[0] as usize;
            let mut name_buf = vec![0u8; name_len];
            reader.read_exact(&mut name_buf)?;
            Some(name_buf)
        } else {
            None
        };

        entries.push(IdAccess { id, access, name });
        computed_mask |= (access & !NAME_IS_USER) as u8;
    }

    Ok((entries, computed_mask & !NO_ENTRY))
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
pub fn recv_rsync_acl<R: Read + ?Sized>(reader: &mut R) -> io::Result<RecvAclResult> {
    let ndx_plus_one = read_varint(reader)?;
    let ndx = ndx_plus_one - 1;

    if ndx >= 0 {
        return Ok(RecvAclResult::CacheHit(ndx as u32));
    }

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
        let (entries, computed_mask) = recv_ida_entries(reader)?;
        acl.names = entries;

        // upstream: acls.c recv_rsync_acl() sets mask_obj from computed_mask
        // when named entries are present but no explicit mask was transmitted
        if !acl.names.is_empty() && acl.mask_obj == NO_ENTRY {
            acl.mask_obj = computed_mask;
        }
    }

    Ok(RecvAclResult::Literal(acl))
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
pub fn recv_acl<R: Read + ?Sized>(
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

/// Receives a single rsync ACL from the wire and stores it in the cache.
///
/// Returns the cache index for the received ACL. If the sender referenced
/// a previously cached ACL, validates the index and returns it. If literal
/// ACL data was sent, stores it in the cache and returns the new index.
///
/// # Errors
///
/// Returns an error if the cache index is out of range (sender sent an
/// index beyond what has been cached so far).
///
/// # Upstream Reference
///
/// Mirrors `recv_rsync_acl()` in `acls.c` lines 731-783, which both
/// reads from wire and appends to `racl_list`, returning the index.
fn recv_rsync_acl_cached<R: Read + ?Sized>(
    reader: &mut R,
    acl_type: AclType,
    cache: &mut AclCache,
) -> io::Result<u32> {
    let result = recv_rsync_acl(reader)?;

    match result {
        RecvAclResult::CacheHit(ndx) => {
            // upstream: acls.c:738 validates ndx < racl_list->count
            let count = match acl_type {
                AclType::Access => cache.access_count(),
                AclType::Default => cache.default_count(),
            };
            if ndx as usize >= count {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "recv_acl_index: {} ACL index {} > {}",
                        match acl_type {
                            AclType::Access => "access",
                            AclType::Default => "default",
                        },
                        ndx,
                        count,
                    ),
                ));
            }
            Ok(ndx)
        }
        RecvAclResult::Literal(acl) => {
            let ndx = match acl_type {
                AclType::Access => cache.store_access(acl),
                AclType::Default => cache.store_default(acl),
            };
            Ok(ndx)
        }
    }
}

/// Receives ACL data for a file entry, storing results in the cache.
///
/// Reads the access ACL from the wire, and for directories also reads
/// the default ACL. Literal ACL data is stored in the cache. Returns
/// the cache indices for the received ACLs.
///
/// This is the main entry point for ACL reception during flist reading.
/// Symlinks are excluded from ACL processing, matching upstream behavior.
///
/// # Arguments
///
/// * `reader` - Input stream
/// * `is_directory` - Whether this entry is a directory (controls default ACL)
/// * `cache` - ACL cache for storing received ACL definitions
///
/// # Returns
///
/// Tuple of (access_acl_index, optional_default_acl_index).
///
/// # Upstream Reference
///
/// Mirrors `receive_acl()` in `acls.c` lines 786-792.
pub fn receive_acl_cached<R: Read + ?Sized>(
    reader: &mut R,
    is_directory: bool,
    cache: &mut AclCache,
) -> io::Result<(u32, Option<u32>)> {
    let access_ndx = recv_rsync_acl_cached(reader, AclType::Access, cache)?;

    let default_ndx = if is_directory {
        Some(recv_rsync_acl_cached(reader, AclType::Default, cache)?)
    } else {
        None
    };

    Ok((access_ndx, default_ndx))
}
