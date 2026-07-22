#![allow(unsafe_code)]

//! DACL read/write operations and [`RsyncAcl`] conversion helpers.

use std::io;
use std::path::Path;
use std::ptr;

use protocol::acl::{AclCache, IdAccess, NO_ENTRY, RsyncAcl};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAce, DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSidSubAuthority,
    GetSidSubAuthorityCount, InitializeAcl, IsValidSid, LookupAccountNameW, LookupAccountSidW,
    PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE, SidTypeAlias, SidTypeGroup, SidTypeWellKnownGroup,
};
// upstream: WinNT.h - ACCESS_ALLOWED_ACE_TYPE is the ACE-type discriminant byte (0x0)
// for an allow ACE; in windows-rs 0.62 it lives under SystemServices, not Security.
use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows::core::{PCWSTR, PWSTR};

use super::common::{
    OwnedSecurityDescriptor, access_mask_to_rsync_perms, is_unsupported,
    rsync_perms_to_access_mask, to_wide, warn_dropped_aces, win32_error,
};
use crate::AclIdMapper;
use crate::MetadataError;

/// Reads the DACL for `path` and returns it together with the owning
/// security descriptor. The descriptor must outlive any pointers into the
/// DACL.
///
/// # Errors
///
/// Returns [`MetadataError`] when the underlying Win32 call fails. Errors
/// indicating "filesystem does not support ACLs" map to `Ok` with a null
/// DACL pointer to mirror upstream's `no_acl_syscall_error()` filter.
pub(super) fn read_dacl(path: &Path) -> Result<(OwnedSecurityDescriptor, *mut ACL), MetadataError> {
    let wide = to_wide(path);
    let mut pdacl: *mut ACL = ptr::null_mut();
    let mut psd = PSECURITY_DESCRIPTOR(ptr::null_mut());

    // SAFETY: `wide` is NUL-terminated; out-pointers live for the entire
    // call. `GetNamedSecurityInfoW` allocates `psd` via `LocalAlloc`; we
    // wrap it in `OwnedSecurityDescriptor` immediately so the buffer is
    // released even on early returns from this function.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut pdacl),
            None,
            &mut psd,
        )
    };

    let owned = OwnedSecurityDescriptor { pd: psd };
    if status != WIN32_ERROR(0) {
        if is_unsupported(status) {
            return Ok((owned, ptr::null_mut()));
        }
        return Err(MetadataError::new(
            "GetNamedSecurityInfoW",
            path,
            win32_error("GetNamedSecurityInfoW", status),
        ));
    }
    Ok((owned, pdacl))
}

/// Resolves a Windows SID into a synthetic uid/gid plus account name.
///
/// Returns `None` if the SID cannot be looked up.
fn sid_to_id_access(psid: PSID) -> Option<(u32, bool, Vec<u8>)> {
    if psid.0.is_null() {
        return None;
    }
    // SAFETY: `psid` came from an ACE that the kernel already validated.
    if unsafe { !IsValidSid(psid).as_bool() } {
        return None;
    }

    let mut name_len: u32 = 0;
    let mut domain_len: u32 = 0;
    let mut sid_type = SID_NAME_USE::default();

    // SAFETY: First call gathers buffer sizes via the documented
    // null-buffer pattern. The function returns FALSE and sets
    // ERROR_INSUFFICIENT_BUFFER when the buffers are absent.
    unsafe {
        let _ = LookupAccountSidW(
            PCWSTR::null(),
            psid,
            None,
            &mut name_len,
            None,
            &mut domain_len,
            &mut sid_type,
        );
    }
    if name_len == 0 {
        return None;
    }

    let mut name_buf = vec![0u16; name_len as usize];
    let mut domain_buf = vec![0u16; domain_len.max(1) as usize];

    // SAFETY: Buffers are sized per the previous call's output values.
    let ok = unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            psid,
            Some(PWSTR(name_buf.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_len,
            &mut sid_type,
        )
    };
    if ok.is_err() {
        return None;
    }

    // SAFETY: `psid` is valid; the count byte is reachable for any valid SID.
    let sub_count = unsafe { *GetSidSubAuthorityCount(psid) };
    if sub_count == 0 {
        return None;
    }
    // SAFETY: `sub_count - 1` is in-range by construction.
    let rid = unsafe { *GetSidSubAuthority(psid, u32::from(sub_count - 1)) };

    let is_group =
        sid_type == SidTypeGroup || sid_type == SidTypeAlias || sid_type == SidTypeWellKnownGroup;

    let trimmed: Vec<u16> = name_buf.iter().take(name_len as usize).copied().collect();
    let name_str = String::from_utf16_lossy(&trimmed);
    Some((rid, is_group, name_str.into_bytes()))
}

/// Iterates the ACEs of a DACL and converts them into a [`RsyncAcl`].
pub(super) fn dacl_to_rsync_acl(pdacl: *mut ACL) -> RsyncAcl {
    let mut acl = RsyncAcl::new();
    if pdacl.is_null() {
        return acl;
    }

    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: `pdacl` is non-null and points to a kernel-validated ACL.
    let res = unsafe {
        GetAclInformation(
            pdacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if res.is_err() {
        return acl;
    }

    for index in 0..info.AceCount {
        let mut ace_ptr: *mut core::ffi::c_void = ptr::null_mut();
        // SAFETY: index is bounded by AceCount; out-pointer is valid.
        let ok = unsafe { GetAce(pdacl, index, &mut ace_ptr) };
        if ok.is_err() || ace_ptr.is_null() {
            continue;
        }

        // SAFETY: All ACE_HEADER variants share the leading header fields,
        // so it is safe to read AceType through a header reference.
        let header = unsafe { &*(ace_ptr.cast::<ACE_HEADER>()) };

        if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8 {
            // Deny ACEs and audit ACEs cannot be expressed in the
            // POSIX-style rsync wire format and are dropped, matching
            // upstream's lossy cross-platform behaviour.
            continue;
        }

        // SAFETY: `AceType == ACCESS_ALLOWED_ACE_TYPE` guarantees the
        // ACE layout matches `ACCESS_ALLOWED_ACE`; `SidStart` marks the
        // offset of the embedded SID.
        let allowed = unsafe { &*(ace_ptr.cast::<ACCESS_ALLOWED_ACE>()) };
        let mask = allowed.Mask;
        let sid_start_addr = std::ptr::addr_of!(allowed.SidStart) as *mut _;
        let psid = PSID(sid_start_addr);

        let perms = access_mask_to_rsync_perms(mask);
        if perms == 0 {
            continue;
        }

        if let Some((rid, is_group, name)) = sid_to_id_access(psid) {
            let entry = if is_group {
                IdAccess::group_with_name(rid, u32::from(perms), name)
            } else {
                IdAccess::user_with_name(rid, u32::from(perms), name)
            };
            acl.names.push(entry);
        }
    }

    acl
}

/// Reads the DACL for `path` and converts it to an [`RsyncAcl`].
///
/// The unnamed `user_obj`/`group_obj`/`other_obj` slots are derived from
/// the file mode bits because NTFS does not expose POSIX permission bits
/// separately. Named ACEs come from the DACL.
///
/// # Upstream Reference
///
/// Mirrors `get_rsync_acl()` in `acls.c` lines 472-536. The fall-back to
/// `RsyncAcl::from_mode()` matches upstream's behaviour when no extended
/// ACL is available.
pub fn get_rsync_acl(path: &Path, mode: u32, is_default: bool) -> RsyncAcl {
    if is_default {
        // upstream: acls.c:472-486 - default ACLs apply to directories on
        // POSIX systems only. NTFS has inherited ACEs but no separate
        // "default ACL" wire entry.
        return RsyncAcl::new();
    }

    match read_dacl(path) {
        Ok((sd, pdacl)) => {
            let mut acl = if pdacl.is_null() {
                RsyncAcl::from_mode(mode)
            } else {
                dacl_to_rsync_acl(pdacl)
            };
            // Keep the descriptor alive across the conversion and drop
            // it explicitly here so the DACL pointer remains valid above.
            drop(sd);

            if acl.user_obj == NO_ENTRY {
                acl.user_obj = ((mode >> 6) & 7) as u8;
            }
            if acl.group_obj == NO_ENTRY {
                acl.group_obj = ((mode >> 3) & 7) as u8;
            }
            if acl.other_obj == NO_ENTRY {
                acl.other_obj = (mode & 7) as u8;
            }
            acl
        }
        Err(_) => RsyncAcl::from_mode(mode),
    }
}

/// Applies parsed ACLs from an [`AclCache`] to a destination file.
///
/// # Errors
///
/// Returns [`MetadataError`] on unrecoverable Win32 failures.
///
/// # Upstream Reference
///
/// Mirrors `set_acl()` in `acls.c` lines 930-1001 and the receiver flow
/// that consumes the ACL cache populated during file-list reception.
#[allow(clippy::module_name_repetitions)]
pub fn apply_acls_from_cache(
    destination: &Path,
    cache: &AclCache,
    access_ndx: u32,
    default_ndx: Option<u32>,
    follow_symlinks: bool,
    mode: Option<u32>,
    id_map: Option<&AclIdMapper>,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    let _ = default_ndx; // Default ACLs are POSIX-only; ignored on Windows.
    // Windows resolves ACL principals by SID/name, not numeric uid/gid, so the
    // POSIX id-list remapper does not apply here.
    let _ = id_map;

    let Some(acl) = cache.get_access(access_ndx) else {
        return Ok(());
    };
    let reconstructed = reconstruct_acl(acl, mode);
    apply_rsync_acl_to_path(destination, &reconstructed)
}

/// Stores parsed ACLs from an [`AclCache`] into `--fake-super` xattrs.
///
/// `--fake-super`'s `%aacl`/`%dacl` stashing is a POSIX-ACL-only mechanism
/// (upstream: `acls.c`'s `am_root < 0` branch operates on `SMB_ACL_T`, which
/// has no Windows equivalent). Windows ACLs are already persisted via their
/// own SDDL xattr (`WINDOWS_SDDL_XATTR_NAME`), so this falls straight through
/// to the normal apply path unchanged.
///
/// # Errors
///
/// Returns [`MetadataError`] on unrecoverable Win32 failures.
#[allow(clippy::module_name_repetitions)]
pub fn store_acls_via_fake_super(
    destination: &Path,
    cache: &AclCache,
    access_ndx: u32,
    default_ndx: Option<u32>,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    apply_acls_from_cache(
        destination,
        cache,
        access_ndx,
        default_ndx,
        follow_symlinks,
        None,
        None,
    )
}

/// Returns the umask-derived default permissions for `dir`.
///
/// Windows lacks POSIX default ACLs, so this returns `ACCESSPERMS & ~umask`
/// without emitting `--debug=ACL` output. Mirrors upstream's `#ifdef
/// SUPPORT_ACLS` guard at `generator.c:1337-1340`, which only consults the
/// directory's default ACL on POSIX targets.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn default_perms_for_dir(dir: &Path, orig_umask: u32) -> u32 {
    let _ = dir;
    0o777u32 & !(orig_umask & 0o777)
}

/// Restores stripped permission entries from `mode`, mirroring the
/// receiver-side logic in `acl_exacl::reconstruct_acl`.
///
/// # Upstream Reference
///
/// Mirrors `change_sacl_perms()` in `acls.c` lines 857-933.
pub(super) fn reconstruct_acl(acl: &RsyncAcl, mode: Option<u32>) -> RsyncAcl {
    let mut result = acl.clone();
    if let Some(mode) = mode {
        if result.user_obj == NO_ENTRY {
            result.user_obj = ((mode >> 6) & 7) as u8;
        }
        if result.group_obj == NO_ENTRY {
            result.group_obj = ((mode >> 3) & 7) as u8;
        }
        if result.other_obj == NO_ENTRY {
            result.other_obj = (mode & 7) as u8;
        }
        if !result.names.is_empty() && result.mask_obj == NO_ENTRY {
            result.mask_obj = ((mode >> 3) & 7) as u8;
        }
    }
    result
}

/// A per-file audit record of ACL entries whose principal could not be
/// resolved to a Windows SID during apply.
///
/// Cross-domain transfers routinely carry principals with no counterpart on
/// the destination host. Rather than discarding those entries behind a single
/// rate-limited warning, each dropped entry is captured with enough identity
/// (account name plus synthetic uid/gid) to give operators a complete
/// per-file trail of exactly what was lost.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct DroppedAces {
    /// Human-readable identifiers of the dropped entries, in ACL order.
    pub(super) descriptions: Vec<String>,
}

impl DroppedAces {
    /// Returns `true` when no entry was dropped.
    pub(super) fn is_empty(&self) -> bool {
        self.descriptions.is_empty()
    }

    /// Records a dropped entry, capturing enough identity to audit the loss.
    fn record(&mut self, entry: &IdAccess) {
        let kind = if entry.is_user() { "uid" } else { "gid" };
        let desc = match entry.name.as_ref() {
            Some(name) => format!("{} ({kind} {})", String::from_utf8_lossy(name), entry.id),
            None => format!("{kind} {}", entry.id),
        };
        self.descriptions.push(desc);
    }
}

/// Resolves each named ACE in `acl` to a Windows SID and access mask.
///
/// Entries whose principal cannot be resolved (missing name, non-UTF-8 name,
/// or an account-name lookup that fails on this host) are not silently
/// discarded: they are recorded in the returned [`DroppedAces`] so the caller
/// can emit a per-file audit trail. Zero-permission entries are skipped
/// without being reported as dropped, mirroring the read-side perms filter.
pub(super) fn resolve_acl_aces(acl: &RsyncAcl) -> (Vec<Vec<u8>>, Vec<u32>, DroppedAces) {
    let mut sids: Vec<Vec<u8>> = Vec::with_capacity(acl.names.len());
    let mut masks: Vec<u32> = Vec::with_capacity(acl.names.len());
    let mut dropped = DroppedAces::default();

    for entry in acl.names.iter() {
        let Some(name) = entry.name.as_ref() else {
            dropped.record(entry);
            continue;
        };
        let Ok(name_str) = std::str::from_utf8(name) else {
            dropped.record(entry);
            continue;
        };
        let Some(sid_buf) = lookup_sid(name_str) else {
            dropped.record(entry);
            continue;
        };
        let mask = rsync_perms_to_access_mask(entry.permissions() as u8);
        if mask == 0 {
            continue;
        }
        sids.push(sid_buf);
        masks.push(mask);
    }

    (sids, masks, dropped)
}

/// Applies the ACEs in `acl.names` to `path` by building a new DACL with
/// one access-allowed ACE per resolvable named entry.
///
/// Unmappable ACEs are dropped and surfaced through a per-file audit
/// diagnostic naming each lost principal (see [`resolve_acl_aces`]); when no
/// entry survives the mapping, no DACL is written so the destination retains
/// its inherited permissions, matching upstream's lossy semantics.
pub(super) fn apply_rsync_acl_to_path(path: &Path, acl: &RsyncAcl) -> Result<(), MetadataError> {
    if acl.names.is_empty() {
        return Ok(());
    }

    let (sids, masks, dropped) = resolve_acl_aces(acl);

    if !dropped.is_empty() {
        warn_dropped_aces(path, &dropped.descriptions);
    }

    if sids.is_empty() {
        return Ok(());
    }

    // DACL size: header + per-ACE (header + mask + sid - 4-byte sentinel
    // for the inline `SidStart` placeholder).
    let mut dacl_size = std::mem::size_of::<ACL>() as u32;
    for sid in &sids {
        dacl_size += std::mem::size_of::<ACCESS_ALLOWED_ACE>() as u32;
        dacl_size += sid.len() as u32;
        dacl_size -= std::mem::size_of::<u32>() as u32;
    }

    let mut dacl_buf = vec![0u8; dacl_size as usize];
    // SAFETY: Buffer is sized to hold ACL + ACEs; ACL_REVISION is the
    // currently-supported revision constant.
    unsafe {
        InitializeAcl(dacl_buf.as_mut_ptr().cast::<ACL>(), dacl_size, ACL_REVISION).map_err(
            |e| {
                MetadataError::new(
                    "InitializeAcl",
                    path,
                    io::Error::other(format!("InitializeAcl: {e}")),
                )
            },
        )?;
    }

    for (sid, mask) in sids.iter().zip(masks.iter()) {
        // SAFETY: `dacl_buf` points to a valid, initialised ACL with
        // enough capacity for the new ACE; `sid` was validated by
        // `lookup_sid` and contains a self-relative SID buffer.
        unsafe {
            AddAccessAllowedAce(
                dacl_buf.as_mut_ptr().cast::<ACL>(),
                ACL_REVISION,
                *mask,
                PSID(sid.as_ptr() as *mut _),
            )
            .map_err(|e| {
                MetadataError::new(
                    "AddAccessAllowedAce",
                    path,
                    io::Error::other(format!("AddAccessAllowedAce: {e}")),
                )
            })?;
        }
    }

    let wide = to_wide(path);
    // SAFETY: `dacl_buf` outlives the call; `wide` is NUL-terminated.
    let status = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl_buf.as_ptr().cast::<ACL>()),
            None,
        )
    };
    if status != WIN32_ERROR(0) {
        if is_unsupported(status) {
            return Ok(());
        }
        return Err(MetadataError::new(
            "SetNamedSecurityInfoW",
            path,
            win32_error("SetNamedSecurityInfoW", status),
        ));
    }

    Ok(())
}

/// Resolves an account name to a self-contained SID byte buffer suitable
/// for [`AddAccessAllowedAce`].
fn lookup_sid(name: &str) -> Option<Vec<u8>> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sid_size: u32 = 0;
    let mut domain_size: u32 = 0;
    let mut sid_type = SID_NAME_USE::default();

    // SAFETY: First call gathers buffer sizes via the documented
    // null-buffer pattern.
    unsafe {
        let _ = LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            None,
            &mut sid_size,
            None,
            &mut domain_size,
            &mut sid_type,
        );
    }
    if sid_size == 0 {
        return None;
    }

    let mut sid_buf = vec![0u8; sid_size as usize];
    let mut domain_buf = vec![0u16; domain_size.max(1) as usize];
    // SAFETY: Buffers are now correctly sized.
    let ok = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            Some(PSID(sid_buf.as_mut_ptr().cast())),
            &mut sid_size,
            Some(PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_size,
            &mut sid_type,
        )
    };
    if ok.is_err() {
        return None;
    }
    sid_buf.truncate(sid_size as usize);
    Some(sid_buf)
}
