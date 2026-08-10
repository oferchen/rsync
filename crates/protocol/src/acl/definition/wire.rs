//! Wire format reading and writing for ACL definitions.
//!
//! Provides `read_acl_definition` and `write_acl_definition` for parsing
//! and encoding the literal ACL body that follows a cache-miss index.
//!
//! # Wire Format
//!
//! ```text
//! flags      : u8       // XMIT_* bits indicating which standard entries exist
//! [user_obj] : varint   // owner permissions (if XMIT_USER_OBJ)
//! [group_obj]: varint   // owning group permissions (if XMIT_GROUP_OBJ)
//! [mask_obj] : varint   // mask permissions (if XMIT_MASK_OBJ)
//! [other_obj]: varint   // world permissions (if XMIT_OTHER_OBJ)
//! [ida_list] : ...      // named user/group entries (if XMIT_NAME_LIST)
//! ```
//!
//! # Upstream Reference
//!
//! - `acls.c` lines 610-647: `send_rsync_acl()`
//! - `acls.c` lines 740-800: `recv_rsync_acl()`

use std::io::{self, Read};

use crate::varint::read_varint;

use super::super::constants::{
    XMIT_GROUP_OBJ, XMIT_MASK_OBJ, XMIT_NAME_LIST, XMIT_OTHER_OBJ, XMIT_USER_OBJ,
};
use super::super::wire::recv_ida_entries;
use super::convert::AclDefinition;
use super::types::{AclEntry, AclPerms, AclTag};

/// Reads an ACL definition from the wire.
///
/// Parses the literal ACL data that follows a cache-miss index (ndx < 0)
/// during file list transfer. Reads the flags byte, standard permission
/// entries, and named user/group entries, returning a unified
/// `AclDefinition` with all entries as `AclEntry` values.
///
/// This function reads the ACL body only - the caller must have already
/// read and dispatched on the cache index varint. Use `recv_rsync_acl`
/// for the full index-or-literal dispatch.
///
/// # Upstream Reference
///
/// Mirrors the literal-data branch of `recv_rsync_acl()` in `acls.c`
/// lines 740-800.
pub fn read_acl_definition<R: Read>(reader: &mut R) -> io::Result<AclDefinition> {
    let mut flags_buf = [0u8; 1];
    reader.read_exact(&mut flags_buf)?;
    let flags = flags_buf[0];

    let mut entries = Vec::new();
    let mut explicit_mask = false;

    if flags & XMIT_USER_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::UserObj, AclPerms::from_bits(perms)));
    }
    if flags & XMIT_GROUP_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::GroupObj, AclPerms::from_bits(perms)));
    }
    if flags & XMIT_MASK_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::Mask, AclPerms::from_bits(perms)));
        explicit_mask = true;
    }
    if flags & XMIT_OTHER_OBJ != 0 {
        let perms = read_varint(reader)? as u8;
        entries.push(AclEntry::new(AclTag::Other, AclPerms::from_bits(perms)));
    }

    if flags & XMIT_NAME_LIST != 0 {
        let (ida_entries, computed_mask) = recv_ida_entries(reader)?;

        for ida in ida_entries.iter() {
            let tag = if ida.is_user() {
                AclTag::User(ida.id)
            } else {
                AclTag::Group(ida.id)
            };
            let perms = AclPerms::from_bits(ida.permissions() as u8);
            entries.push(AclEntry::new(tag, perms));
        }

        // upstream: acls.c recv_rsync_acl() sets mask from computed value
        // when named entries exist but no explicit mask was transmitted
        if !ida_entries.is_empty() && !explicit_mask {
            entries.push(AclEntry::new(
                AclTag::Mask,
                AclPerms::from_bits(computed_mask),
            ));
        }
    }

    Ok(AclDefinition {
        entries,
        mask_set: explicit_mask,
    })
}

/// Writes an ACL definition to the wire.
///
/// Encodes the ACL entries as the flags byte followed by standard entries
/// and named user/group entries. This writes the ACL body only - the
/// caller is responsible for writing the cache index varint first.
///
/// # Upstream Reference
///
/// Mirrors the literal-data branch of `send_rsync_acl()` in `acls.c`
/// lines 610-647.
pub fn write_acl_definition<W: io::Write>(
    writer: &mut W,
    definition: &AclDefinition,
) -> io::Result<()> {
    use super::super::entry::{IdAccess, IdaEntries};
    use super::super::wire::send_ida_entries;
    use crate::varint::write_varint;

    let mut user_obj: Option<u8> = None;
    let mut group_obj: Option<u8> = None;
    let mut mask_obj: Option<u8> = None;
    let mut other_obj: Option<u8> = None;
    let mut named = IdaEntries::new();

    for entry in &definition.entries {
        match entry.tag {
            AclTag::UserObj => user_obj = Some(entry.perms.bits()),
            AclTag::GroupObj => group_obj = Some(entry.perms.bits()),
            AclTag::Mask => mask_obj = Some(entry.perms.bits()),
            AclTag::Other => other_obj = Some(entry.perms.bits()),
            AclTag::User(uid) => {
                named.push(IdAccess::user(uid, u32::from(entry.perms.bits())));
            }
            AclTag::Group(gid) => {
                named.push(IdAccess::group(gid, u32::from(entry.perms.bits())));
            }
        }
    }

    let mut flags = 0u8;
    if user_obj.is_some() {
        flags |= XMIT_USER_OBJ;
    }
    if group_obj.is_some() {
        flags |= XMIT_GROUP_OBJ;
    }
    if mask_obj.is_some() {
        flags |= XMIT_MASK_OBJ;
    }
    if other_obj.is_some() {
        flags |= XMIT_OTHER_OBJ;
    }
    if !named.is_empty() {
        flags |= XMIT_NAME_LIST;
    }

    writer.write_all(&[flags])?;

    if let Some(perms) = user_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if let Some(perms) = group_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if let Some(perms) = mask_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if let Some(perms) = other_obj {
        write_varint(writer, i32::from(perms))?;
    }
    if !named.is_empty() {
        send_ida_entries(writer, &named, false)?;
    }

    Ok(())
}
