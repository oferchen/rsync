#![allow(unsafe_code)]

//! SDDL grammar parse/format helpers plus the SDDL round-trip wrappers
//! around `ConvertSecurityDescriptorToStringSecurityDescriptorW` and
//! `ConvertStringSecurityDescriptorToSecurityDescriptorW`.

use std::io;
use std::path::Path;
use std::ptr;

use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
    GetSecurityDescriptorGroup, GetSecurityDescriptorOwner, GetSecurityDescriptorSacl,
    OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SACL_SECURITY_INFORMATION,
};
use windows::core::{PCWSTR, PWSTR};

use super::common::{
    OwnedLocalWString, OwnedSecurityDescriptor, RSYNC_PERM_EXECUTE, RSYNC_PERM_READ,
    RSYNC_PERM_WRITE, access_mask_to_rsync_perms, to_wide, win32_error,
};

/// SDDL "Everyone" well-known SID alias.
pub(super) const SDDL_EVERYONE: &str = "WD";
/// SDDL "Authenticated Users" alias - mapped to the `other` triplet when
/// no explicit Everyone ACE is present.
pub(super) const SDDL_AUTHENTICATED_USERS: &str = "AU";

/// Computes the security-information mask used by SDDL round-trip helpers.
///
/// Always includes DACL, owner, and group; includes SACL when the caller
/// opts in. SACL access requires `SE_SECURITY_NAME`, so the default keeps
/// it disabled to match the conservative posture of `read_dacl`.
fn sddl_security_info(include_sacl: bool) -> OBJECT_SECURITY_INFORMATION {
    let mut info =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    if include_sacl {
        info |= SACL_SECURITY_INFORMATION;
    }
    info
}

/// Reads the security descriptor at `path` and returns it serialised as
/// an SDDL string.
///
/// The descriptor includes owner, group, and DACL components. SACL data
/// is not requested because it would require `SE_SECURITY_NAME`, which
/// standard accounts lack. To round-trip SACL entries, use
/// [`write_dacl_sddl`] with an SDDL payload that includes them; the OS
/// applies them only if the calling token holds the privilege.
///
/// # Errors
///
/// Returns [`io::Error`] for Win32 failures. Filesystems that do not
/// support security descriptors (FAT32, network mounts) propagate the
/// underlying error.
///
/// # Upstream Reference
///
/// `GetNamedSecurityInfoW` plus
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW`; see
/// `docs/design/windows-ntfs-acl-support.md` section 4.2.
pub fn read_dacl_sddl(path: &Path) -> io::Result<String> {
    read_sddl_internal(path, false)
}

/// Reads the security descriptor at `path` including the SACL.
///
/// Requires the calling process to hold `SE_SECURITY_NAME`. Without the
/// privilege the call fails with `ERROR_PRIVILEGE_NOT_HELD`.
///
/// # Errors
///
/// Returns [`io::Error`] for Win32 failures.
pub fn read_sddl_with_sacl(path: &Path) -> io::Result<String> {
    read_sddl_internal(path, true)
}

fn read_sddl_internal(path: &Path, include_sacl: bool) -> io::Result<String> {
    let wide = to_wide(path);
    let mut psd = PSECURITY_DESCRIPTOR(ptr::null_mut());
    let info = sddl_security_info(include_sacl);

    // SAFETY: `wide` is NUL-terminated; `psd` lives for the call and is
    // wrapped in `OwnedSecurityDescriptor` immediately so the allocation
    // is released even on early returns.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            info,
            None,
            None,
            None,
            None,
            &mut psd,
        )
    };

    let owned = OwnedSecurityDescriptor { pd: psd };
    if status != WIN32_ERROR(0) {
        return Err(win32_error("GetNamedSecurityInfoW", status));
    }

    let mut string_ptr = PWSTR(ptr::null_mut());
    let mut string_len: u32 = 0;
    // SAFETY: `owned.pd` is a valid kernel-allocated descriptor; the
    // out-pointers are exclusively owned by this stack frame. The
    // function allocates `string_ptr` via `LocalAlloc`; ownership is
    // transferred to `OwnedLocalWString` immediately so the buffer is
    // released even on error paths.
    let convert = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            owned.pd,
            SDDL_REVISION_1,
            info,
            &mut string_ptr,
            Some(&mut string_len),
        )
    };
    let owned_string = OwnedLocalWString { ptr: string_ptr };
    convert.map_err(|e| {
        io::Error::other(format!(
            "ConvertSecurityDescriptorToStringSecurityDescriptorW: {e}"
        ))
    })?;

    if owned_string.ptr.0.is_null() {
        return Err(io::Error::other(
            "ConvertSecurityDescriptorToStringSecurityDescriptorW returned null",
        ));
    }

    // SAFETY: `owned_string.ptr` points to a NUL-terminated UTF-16
    // buffer; `string_len` excludes the terminator.
    let slice = unsafe { std::slice::from_raw_parts(owned_string.ptr.0, string_len as usize) };
    Ok(String::from_utf16_lossy(slice))
}

/// Parses an SDDL string and writes it to `path` as the security
/// descriptor for owner, group, and DACL components.
///
/// The DACL is applied with `PROTECTED_DACL_SECURITY_INFORMATION` so the
/// destination does not silently inherit additional ACEs from its parent,
/// matching the policy laid out in
/// `docs/design/windows-ntfs-acl-support.md` section 5.2.
///
/// SACL entries present in the SDDL string are applied only when the
/// calling token holds `SE_SECURITY_NAME`. Without the privilege the OS
/// silently ignores the SACL component; callers needing strict failure
/// semantics should probe the privilege before calling.
///
/// # Errors
///
/// Returns [`io::Error`] for SDDL parse failures or Win32 failures while
/// applying the descriptor.
///
/// # Upstream Reference
///
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW` plus
/// `SetNamedSecurityInfoW`; see `docs/design/windows-ntfs-acl-support.md`
/// section 4.2.
pub fn write_dacl_sddl(path: &Path, sddl: &str) -> io::Result<()> {
    let sddl_wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut psd = PSECURITY_DESCRIPTOR(ptr::null_mut());

    // SAFETY: `sddl_wide` is NUL-terminated; `psd` is exclusive. The
    // function allocates `psd` via `LocalAlloc`; the wrapper releases it
    // on drop.
    let convert = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_wide.as_ptr()),
            SDDL_REVISION_1,
            &mut psd,
            None,
        )
    };
    let owned = OwnedSecurityDescriptor { pd: psd };
    convert.map_err(|e| {
        io::Error::other(format!(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW: {e}"
        ))
    })?;
    if owned.pd.0.is_null() {
        return Err(io::Error::other(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW returned null",
        ));
    }

    // Extract owner SID, group SID, DACL, and SACL from the parsed
    // descriptor. Each accessor reports whether the component is present.
    let mut owner_sid = PSID(ptr::null_mut());
    let mut owner_defaulted = windows::core::BOOL(0);
    // SAFETY: `owned.pd` is non-null, owned, and currently the only
    // reader of the structure.
    unsafe {
        GetSecurityDescriptorOwner(owned.pd, &mut owner_sid, &mut owner_defaulted)
            .map_err(|e| io::Error::other(format!("GetSecurityDescriptorOwner: {e}")))?;
    }

    let mut group_sid = PSID(ptr::null_mut());
    let mut group_defaulted = windows::core::BOOL(0);
    // SAFETY: see owner branch above.
    unsafe {
        GetSecurityDescriptorGroup(owned.pd, &mut group_sid, &mut group_defaulted)
            .map_err(|e| io::Error::other(format!("GetSecurityDescriptorGroup: {e}")))?;
    }

    let mut dacl_present = windows::core::BOOL(0);
    let mut pdacl: *mut ACL = ptr::null_mut();
    let mut dacl_defaulted = windows::core::BOOL(0);
    // SAFETY: see owner branch above.
    unsafe {
        GetSecurityDescriptorDacl(owned.pd, &mut dacl_present, &mut pdacl, &mut dacl_defaulted)
            .map_err(|e| io::Error::other(format!("GetSecurityDescriptorDacl: {e}")))?;
    }

    let mut sacl_present = windows::core::BOOL(0);
    let mut psacl: *mut ACL = ptr::null_mut();
    let mut sacl_defaulted = windows::core::BOOL(0);
    // SAFETY: see owner branch above.
    unsafe {
        GetSecurityDescriptorSacl(owned.pd, &mut sacl_present, &mut psacl, &mut sacl_defaulted)
            .map_err(|e| io::Error::other(format!("GetSecurityDescriptorSacl: {e}")))?;
    }

    // Compose the security-information mask from components that the
    // SDDL string actually populated. Unmentioned components stay
    // untouched on the destination object.
    let mut info = OBJECT_SECURITY_INFORMATION(0);
    let owner_arg: Option<PSID> = if !owner_sid.0.is_null() {
        info |= OWNER_SECURITY_INFORMATION;
        Some(owner_sid)
    } else {
        None
    };
    let group_arg: Option<PSID> = if !group_sid.0.is_null() {
        info |= GROUP_SECURITY_INFORMATION;
        Some(group_sid)
    } else {
        None
    };
    let dacl_arg: Option<*const ACL> = if dacl_present.as_bool() && !pdacl.is_null() {
        info |= DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        Some(pdacl as *const ACL)
    } else {
        None
    };
    let sacl_arg: Option<*const ACL> = if sacl_present.as_bool() && !psacl.is_null() {
        info |= SACL_SECURITY_INFORMATION;
        Some(psacl as *const ACL)
    } else {
        None
    };

    if info.0 == 0 {
        // SDDL string was syntactically valid but conveyed no
        // components; nothing to write.
        return Ok(());
    }

    let wide = to_wide(path);
    // SAFETY: `owned` keeps the descriptor (and therefore the embedded
    // SIDs and ACLs) alive until the function returns. `wide` is
    // NUL-terminated.
    let status = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            info,
            owner_arg,
            group_arg,
            dacl_arg,
            sacl_arg,
        )
    };
    drop(owned);
    if status != WIN32_ERROR(0) {
        return Err(win32_error("SetNamedSecurityInfoW", status));
    }
    Ok(())
}

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
pub(super) fn sddl_rights_to_perms(rights: &str) -> u8 {
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
/// `FX`) so the result round-trips through [`sddl_rights_to_perms`].
/// Returns an empty string for zero perms; the caller is expected to
/// skip empty ACEs.
pub(super) fn perms_to_sddl_rights(perms: u8) -> String {
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
pub(super) fn split_sddl(sddl: &str) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    fn section<'a>(sddl: &'a str, marker: &str) -> Option<&'a str> {
        let start = sddl.find(marker)?;
        let after = &sddl[start + marker.len()..];
        let mut depth: i32 = 0;
        for (idx, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ':' if depth == 0 && idx >= 1 => {
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
pub(super) struct ParsedAce<'a> {
    pub(super) ace_type: &'a str,
    pub(super) flags: &'a str,
    pub(super) rights: &'a str,
    pub(super) trustee: &'a str,
}

/// Parses the ACE list in a DACL section.
///
/// Each ACE is expected to use the canonical six-field form. ACEs with
/// fewer fields are skipped. The `dacl` argument may carry leading
/// section flags such as `P` or `AI` ahead of the first `(`; those are
/// discarded.
pub(super) fn parse_aces(dacl: &str) -> Vec<ParsedAce<'_>> {
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
