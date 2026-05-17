#![cfg(all(feature = "acl", windows))]
#![allow(unsafe_code)]

//! Windows ACL synchronisation via Win32 `GetNamedSecurityInfoW` and
//! `SetNamedSecurityInfoW`.
//!
//! This module bridges oc-rsync's wire-protocol ACL representation
//! ([`RsyncAcl`]) to NTFS DACLs through the
//! [`windows::Win32::Security::Authorization`] FFI surface. It mirrors the
//! upstream rsync ACL flow on Windows hosts so that `--acls`/`-A`
//! preserves discretionary access control entries when both endpoints
//! support ACL semantics.
//!
//! # Scope
//!
//! - Only the discretionary ACL (DACL) is read and written. The system ACL
//!   (SACL) requires the `SE_SECURITY_NAME` privilege and is intentionally
//!   skipped to avoid surprising privilege escalations on standard
//!   accounts.
//! - SACL preservation, inheritance flag round-tripping, and protected
//!   DACL bits are deliberately left as follow-on work; the current
//!   implementation focuses on Tier 1C beta parity.
//!
//! # SID/UID Mapping
//!
//! Upstream rsync transmits ACEs by numeric uid/gid plus an optional
//! account name string. On Unix the names are looked up with
//! `getpwuid`/`getgrgid`; on Windows there is no POSIX uid/gid, so this
//! module follows a "best-effort" lossy convention:
//!
//! - **Sender:** for each translatable SID, encode the account name and
//!   use the lower sub-authority (RID) as the synthetic uid/gid.
//!   Untranslatable SIDs are dropped, matching upstream's lossy
//!   cross-platform ACL semantics (see `acls.c:902-928`).
//! - **Receiver:** look up the SID for the encoded account name. If no
//!   name was sent or the lookup fails, the ACE is dropped, again
//!   matching upstream's lossy cross-platform semantics.
//!
//! # Upstream Reference
//!
//! - `acls.c:580-668` (`send_rsync_acl`, `send_acl`)
//! - `acls.c:670-800` (`recv_rsync_acl`, `recv_acl`)
//! - `acls.c:830-1000` (`set_acl`, `change_sacl_perms`)

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::Once;

#[cfg(test)]
use protocol::acl::IdaEntries;
use protocol::acl::{AclCache, IdAccess, NO_ENTRY, RsyncAcl};
use windows::Win32::Foundation::{ERROR_NOT_SUPPORTED, HLOCAL, LocalFree, WIN32_ERROR};
use windows::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAce, DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSidSubAuthority,
    GetSidSubAuthorityCount, InitializeAcl, IsValidSid, LookupAccountNameW, LookupAccountSidW,
    PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE, SidTypeAlias, SidTypeGroup, SidTypeWellKnownGroup,
};
use windows::Win32::Storage::FileSystem::{
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
// upstream: WinNT.h - ACCESS_ALLOWED_ACE_TYPE is the ACE-type discriminant byte (0x0)
// for an allow ACE; in windows-rs 0.62 it lives under SystemServices, not Security.
use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows::core::{PCWSTR, PWSTR};

use crate::MetadataError;

/// Permission bit corresponding to the rsync `read` bit (0x4).
const RSYNC_PERM_READ: u8 = 0x4;
/// Permission bit corresponding to the rsync `write` bit (0x2).
const RSYNC_PERM_WRITE: u8 = 0x2;
/// Permission bit corresponding to the rsync `execute` bit (0x1).
const RSYNC_PERM_EXECUTE: u8 = 0x1;

/// Emits a one-time warning about partial ACL application.
///
/// Cross-platform ACL transmission is inherently lossy (POSIX UID/GID vs
/// Windows SIDs); the warning informs operators when a particular file's
/// DACL could not be applied verbatim so they can audit the destination.
fn warn_partial_apply() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!(
            "warning: some ACL entries could not be mapped to Windows SIDs and were dropped \
             (cross-platform ACL transmission is best-effort)"
        );
    });
}

/// Converts a Rust [`Path`] to a NUL-terminated UTF-16 buffer suitable for
/// [`PCWSTR`] arguments.
fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Returns `true` when the underlying error indicates the volume does not
/// support DACLs (e.g. FAT32 mounts) or the path is not addressable.
///
/// upstream: acls.c - `no_acl_syscall_error()` swallows ENOTSUP-style errors.
fn is_unsupported(code: WIN32_ERROR) -> bool {
    // ERROR_NOT_SUPPORTED == 50, ERROR_INVALID_FUNCTION == 1, ERROR_FILE_NOT_FOUND == 2.
    matches!(code, ERROR_NOT_SUPPORTED) || code.0 == 1 || code.0 == 2
}

/// Wraps a Win32 error code into [`io::Error`] with a stable description.
fn win32_error(action: &str, code: WIN32_ERROR) -> io::Error {
    io::Error::other(format!("{action}: Win32 error {}", code.0))
}

/// Holds a Win32-allocated security descriptor.
///
/// The descriptor is owned by the kernel and must be released with
/// [`LocalFree`] once we no longer need to read its DACL pointer. The
/// `Drop` impl performs the release; callers must keep the value alive
/// for the duration of any pointer dereferences derived from it.
struct OwnedSecurityDescriptor {
    pd: PSECURITY_DESCRIPTOR,
}

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.pd.0.is_null() {
            // SAFETY: `pd` was allocated by `GetNamedSecurityInfoW`, which
            // documents that callers must release the buffer with
            // `LocalFree`. We never aliased the pointer outside this struct.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.pd.0)));
            }
        }
    }
}

/// Reads the DACL for `path` and returns it together with the owning
/// security descriptor. The descriptor must outlive any pointers into the
/// DACL.
///
/// # Errors
///
/// Returns [`MetadataError`] when the underlying Win32 call fails. Errors
/// indicating "filesystem does not support ACLs" map to `Ok` with a null
/// DACL pointer to mirror upstream's `no_acl_syscall_error()` filter.
fn read_dacl(path: &Path) -> Result<(OwnedSecurityDescriptor, *mut ACL), MetadataError> {
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

/// Maps Windows file-access mask bits to rsync 3-bit rwx permissions.
///
/// Inheritance and synchronisation flags are intentionally collapsed
/// into the rwx triplet because the rsync wire protocol cannot represent
/// them.
fn access_mask_to_rsync_perms(mask: u32) -> u8 {
    let mut bits: u8 = 0;
    if mask & FILE_GENERIC_READ.0 == FILE_GENERIC_READ.0 {
        bits |= RSYNC_PERM_READ;
    }
    if mask & FILE_GENERIC_WRITE.0 == FILE_GENERIC_WRITE.0 {
        bits |= RSYNC_PERM_WRITE;
    }
    if mask & FILE_GENERIC_EXECUTE.0 == FILE_GENERIC_EXECUTE.0 {
        bits |= RSYNC_PERM_EXECUTE;
    }
    bits
}

/// Reverse of [`access_mask_to_rsync_perms`]: builds a Win32 access mask.
fn rsync_perms_to_access_mask(perms: u8) -> u32 {
    let mut mask: u32 = 0;
    if perms & RSYNC_PERM_READ != 0 {
        mask |= FILE_GENERIC_READ.0;
    }
    if perms & RSYNC_PERM_WRITE != 0 {
        mask |= FILE_GENERIC_WRITE.0;
    }
    if perms & RSYNC_PERM_EXECUTE != 0 {
        mask |= FILE_GENERIC_EXECUTE.0;
    }
    mask
}

/// Iterates the ACEs of a DACL and converts them into a [`RsyncAcl`].
fn dacl_to_rsync_acl(pdacl: *mut ACL) -> RsyncAcl {
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

/// Synchronises the DACL from `source` to `destination`.
///
/// Reads the source's DACL, encodes it as a [`RsyncAcl`], and re-applies
/// it to the destination. Symlinks are not followed when
/// `follow_symlinks` is `false`, matching the POSIX path's contract.
///
/// # Errors
///
/// Returns [`MetadataError`] on Win32 failures. Filesystems reporting no
/// ACL support are silently treated as success.
///
/// # Upstream Reference
///
/// Combines `acls.c:get_rsync_acl()` and `set_acl()`.
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    if !source.exists() {
        return Err(MetadataError::new(
            "read ACL",
            source,
            io::Error::new(io::ErrorKind::NotFound, "source does not exist"),
        ));
    }

    let (sd, pdacl) = read_dacl(source)?;
    if pdacl.is_null() {
        drop(sd);
        return Ok(());
    }

    let acl = dacl_to_rsync_acl(pdacl);
    drop(sd);
    if acl.names.is_empty() {
        return Ok(());
    }

    apply_rsync_acl_to_path(destination, &acl)
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
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    let _ = default_ndx; // Default ACLs are POSIX-only; ignored on Windows.

    let Some(acl) = cache.get_access(access_ndx) else {
        return Ok(());
    };
    let reconstructed = reconstruct_acl(acl, mode);
    apply_rsync_acl_to_path(destination, &reconstructed)
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
fn reconstruct_acl(acl: &RsyncAcl, mode: Option<u32>) -> RsyncAcl {
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

/// Applies the ACEs in `acl.names` to `path` by building a new DACL with
/// one access-allowed ACE per resolvable named entry.
///
/// Unmappable ACEs are silently dropped; when no entry survives the
/// mapping, no DACL is written so the destination retains its inherited
/// permissions, matching upstream's lossy semantics.
fn apply_rsync_acl_to_path(path: &Path, acl: &RsyncAcl) -> Result<(), MetadataError> {
    if acl.names.is_empty() {
        return Ok(());
    }

    let mut sids: Vec<Vec<u8>> = Vec::with_capacity(acl.names.len());
    let mut masks: Vec<u32> = Vec::with_capacity(acl.names.len());
    let mut dropped = false;

    for entry in acl.names.iter() {
        let Some(name) = entry.name.as_ref() else {
            dropped = true;
            continue;
        };
        let Ok(name_str) = std::str::from_utf8(name) else {
            dropped = true;
            continue;
        };
        let Some(sid_buf) = lookup_sid(name_str) else {
            dropped = true;
            continue;
        };
        let mask = rsync_perms_to_access_mask(entry.permissions() as u8);
        if mask == 0 {
            continue;
        }
        sids.push(sid_buf);
        masks.push(mask);
    }

    if dropped {
        warn_partial_apply();
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

/// Diagnostic helper exposed for unit tests: returns whether a given
/// [`IdaEntries`] has any name annotation. Keeps the test surface stable
/// even if internal helpers are refactored.
#[cfg(test)]
fn entries_have_names(entries: &IdaEntries) -> bool {
    entries.iter().any(|e| e.name.is_some())
}

/// SDDL "Everyone" well-known SID alias.
const SDDL_EVERYONE: &str = "WD";
/// SDDL "Authenticated Users" alias - mapped to the `other` triplet when
/// no explicit Everyone ACE is present.
const SDDL_AUTHENTICATED_USERS: &str = "AU";

/// Decodes an SDDL rights string into rsync rwx permission bits.
///
/// Recognised single-letter tokens follow Microsoft's SDDL grammar:
///
/// - `FA` (file all) -> rwx
/// - `FR` (file generic read) -> r
/// - `FW` (file generic write) -> w
/// - `FX` (file generic execute) -> x
/// - `GA`/`GR`/`GW`/`GX` (generic all/read/write/execute) -> same mapping
///
/// Hex masks (`0x...`) are decoded via [`access_mask_to_rsync_perms`].
/// Unknown tokens contribute zero bits, matching the design doc's
/// "non-rwx access bits collapsed" rule.
fn sddl_rights_to_perms(rights: &str) -> u8 {
    if let Some(hex) = rights
        .strip_prefix("0x")
        .or_else(|| rights.strip_prefix("0X"))
    {
        if let Ok(mask) = u32::from_str_radix(hex, 16) {
            return access_mask_to_rsync_perms(mask);
        }
        return 0;
    }
    let mut perms: u8 = 0;
    let bytes = rights.as_bytes();
    let mut idx = 0;
    while idx + 1 < bytes.len() {
        let token = &rights[idx..idx + 2];
        match token {
            "FA" | "GA" => perms |= RSYNC_PERM_READ | RSYNC_PERM_WRITE | RSYNC_PERM_EXECUTE,
            "FR" | "GR" => perms |= RSYNC_PERM_READ,
            "FW" | "GW" => perms |= RSYNC_PERM_WRITE,
            "FX" | "GX" => perms |= RSYNC_PERM_EXECUTE,
            _ => {}
        }
        idx += 2;
    }
    perms
}

/// Encodes rsync rwx permission bits as an SDDL rights string.
///
/// Always emits the canonical two-letter file-access tokens (`FR`, `FW`,
/// `FX`) so the result round-trips through
/// [`sddl_rights_to_perms`]. Returns an empty string for zero perms; the
/// caller is expected to skip empty ACEs.
fn perms_to_sddl_rights(perms: u8) -> String {
    let mut out = String::with_capacity(6);
    if perms & RSYNC_PERM_READ != 0 {
        out.push_str("FR");
    }
    if perms & RSYNC_PERM_WRITE != 0 {
        out.push_str("FW");
    }
    if perms & RSYNC_PERM_EXECUTE != 0 {
        out.push_str("FX");
    }
    out
}

/// Splits an SDDL string into its `O:` / `G:` / `D:` / `S:` sections.
///
/// Returns `(owner, group, dacl, sacl)` where each component is `None`
/// when the corresponding header is absent. The `dacl` and `sacl`
/// payloads include the parenthesised ACE list but exclude any trailing
/// section flags (e.g. `P`, `AI`).
fn split_sddl(sddl: &str) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    // Locate each section header at the start of the string or
    // immediately after a closing ACE parenthesis. SDDL grammar
    // guarantees the four headers appear at most once.
    fn section<'a>(sddl: &'a str, marker: &str) -> Option<&'a str> {
        let start = sddl.find(marker)?;
        let after = &sddl[start + marker.len()..];
        // Section ends at the next two-character header (`O:`, `G:`, `D:`,
        // `S:`) that is not nested inside an ACE.
        let mut depth: i32 = 0;
        for (idx, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ':' if depth == 0 && idx >= 1 => {
                    // Header is the character preceding the colon plus
                    // the colon itself. Trim it off the returned slice.
                    let header_start = idx - 1;
                    let prev = after.as_bytes()[header_start];
                    if matches!(prev, b'O' | b'G' | b'D' | b'S') {
                        return Some(after[..header_start].trim());
                    }
                }
                _ => {}
            }
        }
        Some(after.trim())
    }
    (
        section(sddl, "O:"),
        section(sddl, "G:"),
        section(sddl, "D:"),
        section(sddl, "S:"),
    )
}

/// Parsed SDDL ACE: `(type;flags;rights;object_guid;inherit_guid;trustee)`.
struct ParsedAce<'a> {
    ace_type: &'a str,
    flags: &'a str,
    rights: &'a str,
    trustee: &'a str,
}

/// Parses the ACE list in a DACL section.
///
/// Each ACE is expected to use the canonical six-field form. ACEs with
/// fewer fields are skipped. The `dacl` argument may carry leading
/// section flags such as `P` or `AI` ahead of the first `(`; those are
/// discarded.
fn parse_aces(dacl: &str) -> Vec<ParsedAce<'_>> {
    let mut out = Vec::new();
    let mut rest = dacl;
    while let Some(open) = rest.find('(') {
        let Some(close_rel) = rest[open + 1..].find(')') else {
            break;
        };
        let inner = &rest[open + 1..open + 1 + close_rel];
        let fields: Vec<&str> = inner.splitn(6, ';').collect();
        if fields.len() == 6 {
            out.push(ParsedAce {
                ace_type: fields[0],
                flags: fields[1],
                rights: fields[2],
                trustee: fields[5],
            });
        }
        rest = &rest[open + 1 + close_rel + 1..];
    }
    out
}

/// Converts an SDDL security-descriptor string into a POSIX permission
/// mode triplet (rwxrwxrwx, 9 bits in the canonical 0o000-0o777 range).
///
/// Mapping rules (matching `docs/design/windows-ntfs-acl-support.md`
/// section 5.1):
///
/// - The first allow ACE whose trustee matches the descriptor's owner
///   SID supplies the owner rwx bits.
/// - The first allow ACE whose trustee matches the group SID supplies
///   the group rwx bits.
/// - The first allow ACE addressed to `WD` (Everyone) or, in its
///   absence, `AU` (Authenticated Users) supplies the other rwx bits.
///
/// Deny ACEs, inherited ACEs (`AceFlags` carrying `ID`), and access bits
/// outside `FR`/`FW`/`FX`/`FA` are dropped with a one-time warning to
/// reflect documented lossy behaviour. If a triplet has no matching ACE
/// the corresponding three bits remain `0`.
///
/// # Panics
///
/// Never panics. Malformed input returns `0`.
#[must_use]
pub fn dacl_to_posix_mode(sddl: &str) -> u32 {
    let (owner, group, dacl, _sacl) = split_sddl(sddl);
    let Some(dacl) = dacl else {
        return 0;
    };
    let owner = owner.unwrap_or("");
    let group = group.unwrap_or("");

    let mut owner_perms: u8 = 0;
    let mut group_perms: u8 = 0;
    let mut other_perms: u8 = 0;
    let mut owner_seen = false;
    let mut group_seen = false;
    let mut everyone_seen = false;
    let mut authenticated_perms: u8 = 0;
    let mut authenticated_seen = false;
    let mut dropped = false;

    for ace in parse_aces(dacl) {
        if ace.flags.contains("ID") {
            // Inherited ACE: not transmitted per design doc section 5.3.
            dropped = true;
            continue;
        }
        if !ace.ace_type.eq_ignore_ascii_case("A") {
            if ace.ace_type.eq_ignore_ascii_case("D") {
                dropped = true;
            }
            continue;
        }
        let perms = sddl_rights_to_perms(ace.rights);
        if perms == 0 {
            continue;
        }
        if !owner.is_empty() && ace.trustee == owner && !owner_seen {
            owner_perms = perms;
            owner_seen = true;
        } else if !group.is_empty() && ace.trustee == group && !group_seen {
            group_perms = perms;
            group_seen = true;
        } else if ace.trustee == SDDL_EVERYONE && !everyone_seen {
            other_perms = perms;
            everyone_seen = true;
        } else if ace.trustee == SDDL_AUTHENTICATED_USERS && !authenticated_seen {
            authenticated_perms = perms;
            authenticated_seen = true;
        }
    }

    if !everyone_seen && authenticated_seen {
        other_perms = authenticated_perms;
    }

    if dropped {
        warn_partial_apply();
    }

    (u32::from(owner_perms) << 6) | (u32::from(group_perms) << 3) | u32::from(other_perms)
}

/// Generates an SDDL security-descriptor string from a POSIX permission
/// mode and the owning user / group SIDs.
///
/// The emitted DACL contains three explicit allow ACEs, in canonical
/// order:
///
/// 1. Allow ACE for the owner SID with the owner triplet's rwx bits.
/// 2. Allow ACE for the group SID with the group triplet's rwx bits.
/// 3. Allow ACE for `WD` (Everyone) with the other triplet's rwx bits.
///
/// Permission triplets with no bits set are still emitted as empty
/// rights ACEs (`(A;;;;;<SID>)`) so the round-trip preserves the
/// distinction between "no permissions" and "ACE omitted entirely".
///
/// The `P` flag is set on the DACL so parent inheritance cannot silently
/// add ACEs that were never on the source, matching section 5.2 of the
/// design document.
///
/// # Panics
///
/// Never panics.
#[must_use]
pub fn posix_mode_to_dacl(mode: u32, owner_sid: &str, group_sid: &str) -> String {
    let owner_perms = ((mode >> 6) & 0o7) as u8;
    let group_perms = ((mode >> 3) & 0o7) as u8;
    let other_perms = (mode & 0o7) as u8;

    format!(
        "O:{owner}G:{group}D:P(A;;{owner_rights};;;{owner})(A;;{group_rights};;;{group})(A;;{other_rights};;;WD)",
        owner = owner_sid,
        group = group_sid,
        owner_rights = perms_to_sddl_rights(owner_perms),
        group_rights = perms_to_sddl_rights(group_perms),
        other_rights = perms_to_sddl_rights(other_perms),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn perms_round_trip_through_access_mask() {
        for perms in 0u8..=0b111 {
            let mask = rsync_perms_to_access_mask(perms);
            let back = access_mask_to_rsync_perms(mask);
            assert_eq!(back, perms, "round-trip failed for {perms:03b}");
        }
    }

    #[test]
    fn reconstruct_acl_fills_base_entries_from_mode() {
        let stripped = RsyncAcl::default();
        let restored = reconstruct_acl(&stripped, Some(0o751));
        assert_eq!(restored.user_obj, 0o7);
        assert_eq!(restored.group_obj, 0o5);
        assert_eq!(restored.other_obj, 0o1);
    }

    #[test]
    fn reconstruct_acl_keeps_existing_entries() {
        let mut acl = RsyncAcl::default();
        acl.user_obj = 0o4;
        let restored = reconstruct_acl(&acl, Some(0o777));
        assert_eq!(restored.user_obj, 0o4);
        assert_eq!(restored.group_obj, 0o7);
        assert_eq!(restored.other_obj, 0o7);
    }

    #[test]
    fn reconstruct_acl_no_mode_passes_through() {
        let mut acl = RsyncAcl::default();
        acl.user_obj = 0o7;
        let restored = reconstruct_acl(&acl, None);
        assert_eq!(restored.user_obj, 0o7);
        assert_eq!(restored.group_obj, NO_ENTRY);
    }

    #[test]
    fn sync_acls_skips_when_not_following_symlinks() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        File::create(&src).expect("src");
        File::create(&dst).expect("dst");
        let result = sync_acls(&src, &dst, false);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_acls_returns_not_found_for_missing_source() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("missing");
        let dst = dir.path().join("dst");
        File::create(&dst).expect("dst");
        let result = sync_acls(&src, &dst, true);
        assert!(result.is_err());
    }

    #[test]
    fn apply_acls_from_cache_skips_when_not_following() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("file");
        let cache = AclCache::new();
        let result = apply_acls_from_cache(&file, &cache, 0, None, false, None);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_acls_from_cache_missing_index_is_noop() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("file");
        let cache = AclCache::new();
        let result = apply_acls_from_cache(&file, &cache, 99, None, true, Some(0o644));
        assert!(result.is_ok());
    }

    #[test]
    fn apply_acls_from_cache_empty_cache_no_op() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("file");
        let mut cache = AclCache::new();
        let acl = RsyncAcl::from_mode(0o644);
        let ndx = cache.store_access(acl);
        let result = apply_acls_from_cache(&file, &cache, ndx, None, true, Some(0o644));
        assert!(result.is_ok());
    }

    #[test]
    fn get_rsync_acl_default_returns_empty() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("file");
        let acl = get_rsync_acl(&file, 0o644, true);
        assert!(acl.is_empty());
    }

    #[test]
    fn entries_have_names_helper() {
        let mut entries = IdaEntries::new();
        assert!(!entries_have_names(&entries));
        entries.push(IdAccess::user_with_name(1000, 0o5, b"alice".to_vec()));
        assert!(entries_have_names(&entries));
    }

    #[cfg(windows)]
    #[test]
    fn read_dacl_on_temp_file_returns_dacl() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("file");
        let result = read_dacl(&file);
        assert!(result.is_ok(), "read_dacl failed: {:?}", result.err());
        let (sd, pdacl) = result.unwrap();
        // NTFS volumes always return a DACL; ReFS/FAT may return null.
        assert!(!pdacl.is_null() || sd.pd.0.is_null());
    }

    #[cfg(windows)]
    #[test]
    fn sync_acls_round_trips_on_ntfs() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        File::create(&src).expect("src");
        File::create(&dst).expect("dst");
        // No assertion on the contents - inheritance varies between
        // CI runners. We just assert the call does not error on a
        // straightforward NTFS temp file.
        let result = sync_acls(&src, &dst, true);
        assert!(result.is_ok(), "sync_acls failed: {:?}", result.err());
    }

    #[test]
    fn sddl_rights_decode_two_letter_tokens() {
        assert_eq!(
            sddl_rights_to_perms("FA"),
            RSYNC_PERM_READ | RSYNC_PERM_WRITE | RSYNC_PERM_EXECUTE
        );
        assert_eq!(sddl_rights_to_perms("FR"), RSYNC_PERM_READ);
        assert_eq!(sddl_rights_to_perms("FW"), RSYNC_PERM_WRITE);
        assert_eq!(sddl_rights_to_perms("FX"), RSYNC_PERM_EXECUTE);
        assert_eq!(
            sddl_rights_to_perms("FRFX"),
            RSYNC_PERM_READ | RSYNC_PERM_EXECUTE
        );
    }

    #[test]
    fn sddl_rights_decode_hex_mask() {
        let mask = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;
        let token = format!("0x{mask:x}");
        assert_eq!(
            sddl_rights_to_perms(&token),
            RSYNC_PERM_READ | RSYNC_PERM_WRITE
        );
    }

    #[test]
    fn sddl_rights_encode_canonical_order() {
        assert_eq!(perms_to_sddl_rights(0), "");
        assert_eq!(perms_to_sddl_rights(RSYNC_PERM_READ), "FR");
        assert_eq!(
            perms_to_sddl_rights(RSYNC_PERM_READ | RSYNC_PERM_EXECUTE),
            "FRFX"
        );
        assert_eq!(
            perms_to_sddl_rights(RSYNC_PERM_READ | RSYNC_PERM_WRITE | RSYNC_PERM_EXECUTE),
            "FRFWFX"
        );
    }

    #[test]
    fn posix_mode_to_dacl_uses_three_allow_aces_with_protected_flag() {
        let sddl = posix_mode_to_dacl(0o755, "S-1-5-21-100", "S-1-5-21-200");
        assert!(sddl.starts_with("O:S-1-5-21-100"));
        assert!(sddl.contains("G:S-1-5-21-200"));
        assert!(sddl.contains("D:P("));
        // owner gets rwx
        assert!(sddl.contains("(A;;FRFWFX;;;S-1-5-21-100)"));
        // group gets r-x
        assert!(sddl.contains("(A;;FRFX;;;S-1-5-21-200)"));
        // other gets r-x via WD
        assert!(sddl.contains("(A;;FRFX;;;WD)"));
    }

    #[test]
    fn round_trip_mode_755_preserves_rwx_triplet() {
        let owner = "S-1-5-21-1";
        let group = "S-1-5-21-2";
        let sddl = posix_mode_to_dacl(0o755, owner, group);
        let back = dacl_to_posix_mode(&sddl);
        assert_eq!(back, 0o755, "round-trip lost bits; sddl: {sddl}");
    }

    #[test]
    fn round_trip_full_mode_matrix_preserves_rwx() {
        let owner = "S-1-5-21-1000";
        let group = "S-1-5-21-1001";
        for mode in 0o000u32..=0o777u32 {
            let sddl = posix_mode_to_dacl(mode, owner, group);
            let back = dacl_to_posix_mode(&sddl);
            assert_eq!(back, mode, "round-trip lost bits for mode {mode:03o}");
        }
    }

    #[test]
    fn dacl_to_posix_mode_handles_everyone_as_other() {
        let sddl = "O:BAG:SYD:(A;;FA;;;BA)(A;;FRFX;;;SY)(A;;FR;;;WD)";
        let mode = dacl_to_posix_mode(sddl);
        // owner BA -> 7, group SY -> 5, other WD -> 4
        assert_eq!(mode, 0o754);
    }

    #[test]
    fn dacl_to_posix_mode_falls_back_to_authenticated_users() {
        let sddl = "O:BAG:SYD:(A;;FA;;;BA)(A;;FRFX;;;SY)(A;;FRFX;;;AU)";
        let mode = dacl_to_posix_mode(sddl);
        // AU substitutes for missing WD -> other = 5
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn dacl_to_posix_mode_drops_deny_aces() {
        // Deny ACE for owner should be ignored; only the allow ACEs
        // contribute. Lossy: the deny is logged via warn_partial_apply.
        let sddl = "O:BAG:SYD:(D;;FW;;;BA)(A;;FRFX;;;BA)(A;;FR;;;SY)(A;;FR;;;WD)";
        let mode = dacl_to_posix_mode(sddl);
        // owner gets rx (5) since deny is dropped, group r (4), other r (4)
        assert_eq!(mode, 0o544);
    }

    #[test]
    fn dacl_to_posix_mode_drops_inherited_aces() {
        // The (A;ID;...) ACE carries the INHERITED flag and must be
        // skipped per design doc section 5.3.
        let sddl = "O:BAG:SYD:(A;ID;FA;;;BA)(A;;FR;;;BA)(A;;FR;;;SY)(A;;FR;;;WD)";
        let mode = dacl_to_posix_mode(sddl);
        // owner picks up the explicit (non-inherited) r=4 ACE.
        assert_eq!(mode, 0o444);
    }

    #[test]
    fn dacl_to_posix_mode_collapses_non_rwx_bits() {
        // DELETE | WRITE_DAC have no rwx representation; the resulting
        // mode bits for owner should be 0.
        let sddl = "O:BAG:SYD:(A;;0x10000;;;BA)(A;;FR;;;SY)(A;;FR;;;WD)";
        let mode = dacl_to_posix_mode(sddl);
        assert_eq!(mode & 0o700, 0);
        assert_eq!(mode & 0o077, 0o044);
    }

    #[test]
    fn dacl_to_posix_mode_returns_zero_for_missing_dacl() {
        assert_eq!(dacl_to_posix_mode("O:BAG:SY"), 0);
        assert_eq!(dacl_to_posix_mode(""), 0);
    }

    #[test]
    fn split_sddl_separates_owner_group_dacl() {
        let (o, g, d, s) = split_sddl("O:BAG:SYD:(A;;FA;;;BA)");
        assert_eq!(o, Some("BA"));
        assert_eq!(g, Some("SY"));
        assert_eq!(d, Some("(A;;FA;;;BA)"));
        assert_eq!(s, None);
    }

    #[test]
    fn parse_aces_skips_malformed_entries() {
        let aces = parse_aces("(A;;FA;;;BA)(broken)(A;;FR;;;WD)");
        assert_eq!(aces.len(), 2);
        assert_eq!(aces[0].trustee, "BA");
        assert_eq!(aces[1].trustee, "WD");
    }
}
