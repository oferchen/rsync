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

    // First call to determine buffer sizes.
    // SAFETY: Passing null buffers with zero sizes to get required sizes is
    // the documented usage pattern for LookupAccountNameW.
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

    // SAFETY: `sid_buf` and `domain_buf` are correctly sized from the first call.
    // `sid_type` receives the account type classification.
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

    #[cfg(not(windows))]
    #[test]
    fn non_windows_always_returns_none() {
        assert!(name_to_rid("Administrator").is_none());
        assert!(name_to_rid("root").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn administrator_resolves_to_rid_500() {
        // The built-in Administrator account always has RID 500.
        if let Some(rid) = name_to_rid("Administrator") {
            assert_eq!(rid, 500);
        }
        // May return None on systems where Administrator is renamed/disabled.
    }
}
