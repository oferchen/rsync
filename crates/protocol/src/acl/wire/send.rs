//! ACL wire protocol sending functions.
//!
//! Implements the send side of the ACL wire protocol, mirroring
//! upstream rsync's `acls.c` send functions.

use std::io::{self, Write};

use crate::varint::write_varint;

use super::super::constants::{
    XMIT_GROUP_OBJ, XMIT_MASK_OBJ, XMIT_NAME_LIST, XMIT_OTHER_OBJ, XMIT_USER_OBJ,
};
use super::super::entry::{AclCache, IdaEntries, RsyncAcl};
use super::encoding::encode_access;
use super::types::AclType;

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

        let has_name = include_names && entry.name.is_some();
        let encoded = encode_access(entry.access, has_name);
        write_varint(writer, encoded as i32)?;

        // upstream: acls.c send_ida_entries() writes name after access flags
        if has_name {
            if let Some(ref name) = entry.name {
                let len = name.len().min(255);
                writer.write_all(&[len as u8])?;
                writer.write_all(&name[..len])?;
            }
        }
    }

    Ok(())
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
    let cached_index = match acl_type {
        AclType::Access => cache.find_access(acl),
        AclType::Default => cache.find_default(acl),
    };

    // upstream: ndx + 1 convention (0 means literal data follows)
    let ndx = cached_index.map(|i| i as i32).unwrap_or(-1);
    write_varint(writer, ndx + 1)?;

    if cached_index.is_some() {
        return Ok(());
    }

    let acl_clone = acl.clone();
    match acl_type {
        AclType::Access => cache.store_access(acl_clone),
        AclType::Default => cache.store_default(acl_clone),
    };

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
