//! Windows account name to RID resolution.
//!
//! # Windows
//!
//! Uses `LookupAccountNameW` and `GetSidSubAuthority` to convert between
//! Windows account names and their relative identifiers (RIDs).
//!
//! # Other
//!
//! Returns `None` - name resolution is not applicable.
//!
//! # Upstream Reference
//!
//! `clientserver.c:rsync_module()` - uid/gid are resolved to account names
//! and used for impersonation on Windows.

/// Resolves a Windows account name to its relative identifier (RID).
///
/// Accepts `DOMAIN\user` or plain `user` format. Returns `None` on
/// non-Windows platforms or if the account cannot be found.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn name_to_rid(name: &str) -> Option<u32> {
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, LookupAccountNameW, PSID, SID_NAME_USE,
        SidTypeUser,
    };
    use windows::core::PCWSTR;

    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    let mut sid_size: u32 = 0;
    let mut domain_size: u32 = 0;
    let mut sid_type = SID_NAME_USE::default();

    // SAFETY: First call uses null buffers and zero sizes to retrieve the
    // required buffer sizes - this is the documented two-call pattern for
    // LookupAccountNameW.
    unsafe {
        let _ = LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name_wide.as_ptr()),
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

    let mut sid_buf = vec![0_u8; sid_size as usize];
    let mut domain_buf = vec![0_u16; domain_size as usize];

    // SAFETY: Buffers are correctly sized from the first call; `sid_type`
    // receives the account type classification.
    let ok = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name_wide.as_ptr()),
            Some(PSID(sid_buf.as_mut_ptr().cast())),
            &mut sid_size,
            Some(windows::core::PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_size,
            &mut sid_type,
        )
    };

    if ok.is_err() || sid_type != SidTypeUser {
        return None;
    }

    let psid = PSID(sid_buf.as_ptr() as *mut _);

    // SAFETY: `psid` is a valid SID populated by LookupAccountNameW.
    // GetSidSubAuthorityCount returns a pointer to the sub-authority count byte.
    let sub_count = unsafe { *GetSidSubAuthorityCount(psid) };
    if sub_count == 0 {
        return None;
    }

    // SAFETY: The sub-authority index is valid (count - 1). The returned pointer
    // points into the SID buffer which is still alive.
    let rid = unsafe { *GetSidSubAuthority(psid, (sub_count - 1) as u32) };
    Some(rid)
}

/// Non-Windows stub - always returns `None`.
#[cfg(not(windows))]
pub fn name_to_rid(_name: &str) -> Option<u32> {
    None
}

/// Resolves a Windows RID back to its account name.
///
/// Enumerates local user accounts via `NetUserEnum` and matches by RID.
/// Returns `None` on non-Windows platforms or if no account matches.
///
/// upstream: uidlist.c - reverse lookup for uid/gid to name mapping.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn rid_to_account_name(rid: u32) -> Option<String> {
    use windows::Win32::NetworkManagement::NetManagement::{
        FILTER_NORMAL_ACCOUNT, NetApiBufferFree, NetUserEnum, USER_INFO_0,
    };
    use windows::core::PCWSTR;

    let mut buf_ptr: *mut u8 = std::ptr::null_mut();
    let mut entries_read: u32 = 0;
    let mut total_entries: u32 = 0;

    // SAFETY: NetUserEnum with level 0 returns an array of USER_INFO_0 structs.
    // All out-parameter pointers are valid.
    let status = unsafe {
        NetUserEnum(
            PCWSTR::null(),
            0,
            FILTER_NORMAL_ACCOUNT,
            &mut buf_ptr,
            u32::MAX,
            &mut entries_read,
            &mut total_entries,
            None,
        )
    };

    if status != 0 {
        return None;
    }

    let result = if !buf_ptr.is_null() && entries_read > 0 {
        // SAFETY: `buf_ptr` is a valid array of `entries_read` USER_INFO_0 structs
        // allocated by NetUserEnum.
        let users: &[USER_INFO_0] =
            unsafe { std::slice::from_raw_parts(buf_ptr.cast(), entries_read as usize) };

        let mut found = None;
        for user in users {
            // SAFETY: `usri0_name` is a valid PWSTR from NetUserEnum.
            if let Ok(name) = unsafe { user.usri0_name.to_string() } {
                if name_to_rid(&name) == Some(rid) {
                    found = Some(name);
                    break;
                }
            }
        }
        found
    } else {
        None
    };

    if !buf_ptr.is_null() {
        // SAFETY: Buffer was allocated by NetUserEnum and must be freed.
        let _ = unsafe { NetApiBufferFree(Some(buf_ptr.cast())) };
    }

    result
}

/// Non-Windows stub - always returns `None`.
#[cfg(not(windows))]
pub fn rid_to_account_name(_rid: u32) -> Option<String> {
    None
}

/// Resolves a Windows account name and returns both the RID and account type.
///
/// Returns `(rid, is_group)` where `is_group` is `true` for group/alias accounts.
/// Accepts `DOMAIN\name` or plain `name` format.
///
/// upstream: clientserver.c - resolves uid/gid names for privilege dropping.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn lookup_account_info(name: &str) -> Option<(u32, bool)> {
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, LookupAccountNameW, PSID, SID_NAME_USE,
        SidTypeAlias, SidTypeGroup, SidTypeWellKnownGroup,
    };
    use windows::core::PCWSTR;

    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    let mut sid_size: u32 = 0;
    let mut domain_size: u32 = 0;
    let mut sid_type = SID_NAME_USE::default();

    // SAFETY: Passing null buffers with zero sizes to get required sizes.
    unsafe {
        let _ = LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name_wide.as_ptr()),
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

    let mut sid_buf = vec![0_u8; sid_size as usize];
    let mut domain_buf = vec![0_u16; domain_size as usize];

    // SAFETY: Buffers are correctly sized from the first call.
    let ok = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name_wide.as_ptr()),
            Some(PSID(sid_buf.as_mut_ptr().cast())),
            &mut sid_size,
            Some(windows::core::PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_size,
            &mut sid_type,
        )
    };

    if ok.is_err() {
        return None;
    }

    let psid = PSID(sid_buf.as_ptr() as *mut _);

    // SAFETY: `psid` is a valid SID from LookupAccountNameW.
    let sub_count = unsafe { *GetSidSubAuthorityCount(psid) };
    if sub_count == 0 {
        return None;
    }

    // SAFETY: Valid sub-authority index.
    let rid = unsafe { *GetSidSubAuthority(psid, (sub_count - 1) as u32) };
    let is_group =
        sid_type == SidTypeGroup || sid_type == SidTypeAlias || sid_type == SidTypeWellKnownGroup;

    Some((rid, is_group))
}

/// Non-Windows stub - always returns `None`.
#[cfg(not(windows))]
pub fn lookup_account_info(_name: &str) -> Option<(u32, bool)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_account_returns_none() {
        assert!(name_to_rid("nonexistent_user_xyz_99999").is_none());
    }

    #[test]
    fn empty_name_returns_none() {
        assert!(name_to_rid("").is_none());
    }

    #[test]
    fn rid_to_account_name_nonexistent_returns_none() {
        assert!(rid_to_account_name(999_999_999).is_none());
    }

    #[test]
    fn lookup_account_info_nonexistent_returns_none() {
        assert!(lookup_account_info("nonexistent_user_xyz_99999").is_none());
    }

    #[test]
    fn lookup_account_info_empty_returns_none() {
        assert!(lookup_account_info("").is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_stubs_return_none() {
        assert!(name_to_rid("Administrator").is_none());
        assert!(rid_to_account_name(500).is_none());
        assert!(lookup_account_info("Administrator").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn administrator_resolves_to_rid_500() {
        // The built-in Administrator account always has RID 500.
        if let Some(rid) = name_to_rid("Administrator") {
            assert_eq!(rid, 500);
        }
    }

    #[cfg(windows)]
    #[test]
    fn rid_500_resolves_to_administrator() {
        if let Some(name) = rid_to_account_name(500) {
            assert_eq!(name, "Administrator");
        }
    }

    #[cfg(windows)]
    #[test]
    fn lookup_account_info_user_returns_non_group() {
        if let Some((rid, is_group)) = lookup_account_info("Administrator") {
            assert_eq!(rid, 500);
            assert!(!is_group);
        }
    }

    #[cfg(windows)]
    #[test]
    fn lookup_account_info_group_returns_is_group() {
        // "Administrators" is a built-in alias group.
        if let Some((_rid, is_group)) = lookup_account_info("Administrators") {
            assert!(is_group);
        }
    }
}
