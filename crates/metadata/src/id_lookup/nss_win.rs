//! Windows local-group membership for `auth users = @group`.
//!
//! POSIX resolves the authenticating user's group set with `getgrouplist`
//! (see `nss.rs`). Windows has no POSIX group database, so this module
//! enumerates the local groups an account belongs to via
//! `NetUserGetLocalGroups`. The direction matches the Unix path - the *user's*
//! groups, not the members of one named group - so a wildcard token like
//! `@admin*` can be matched against every group the user is in, mirroring
//! upstream `authenticate.c auth_server()`, which admits a client that is a
//! member of the named group.

use std::io;

/// Returns the names of every local group the named user belongs to, including
/// membership inherited through global groups.
///
/// Enumerates membership with `NetUserGetLocalGroups` at info level 0
/// (`LOCALGROUP_USERS_INFO_0`) and passes `LG_INCLUDE_INDIRECT`, so a user who
/// belongs to a global group that is itself a member of a local group is still
/// reported. An account name that does not resolve yields an empty list rather
/// than an error, matching the Unix path (a missing passwd entry is treated as
/// "no groups") and upstream's `user_to_uid()` failure handling.
///
/// The username is passed to the API verbatim; both a bare `user` and a
/// qualified `DOMAIN\user` form are accepted by the local lookup.
///
/// upstream: authenticate.c:283-295 `auth_server()` resolves the user's groups
/// and `wildmatch`es each against the `@group` token.
#[allow(unsafe_code)]
pub fn groups_for_user(username: &str) -> Result<Vec<String>, io::Error> {
    use windows::Win32::NetworkManagement::NetManagement::{
        LG_INCLUDE_INDIRECT, LOCALGROUP_USERS_INFO_0, MAX_PREFERRED_LENGTH, NetApiBufferFree,
        NetUserGetLocalGroups,
    };
    use windows::core::PCWSTR;

    let user_wide: Vec<u16> = username.encode_utf16().chain(std::iter::once(0)).collect();

    let mut buf_ptr: *mut u8 = std::ptr::null_mut();
    let mut entries_read: u32 = 0;
    let mut total_entries: u32 = 0;

    // SAFETY: `user_wide` is a NUL-terminated UTF-16 string; every out-parameter
    // pointer is valid for this stack frame. On success the call allocates
    // `buf_ptr`, which is released below with NetApiBufferFree.
    let status = unsafe {
        NetUserGetLocalGroups(
            PCWSTR::null(),
            PCWSTR(user_wide.as_ptr()),
            0,
            LG_INCLUDE_INDIRECT,
            &mut buf_ptr,
            MAX_PREFERRED_LENGTH,
            &mut entries_read,
            &mut total_entries,
        )
    };

    // NERR_UserNotFound (2221) and ERROR_NO_SUCH_USER (1317): an unknown account
    // has no groups, mirroring the Unix empty-list path rather than erroring.
    if status == 2221 || status == 1317 {
        return Ok(Vec::new());
    }
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }

    let mut names = Vec::new();
    if !buf_ptr.is_null() && entries_read > 0 {
        // SAFETY: on success `buf_ptr` addresses `entries_read` contiguous
        // LOCALGROUP_USERS_INFO_0 structs allocated by the call above.
        let infos: &[LOCALGROUP_USERS_INFO_0] =
            unsafe { std::slice::from_raw_parts(buf_ptr.cast(), entries_read as usize) };
        for info in infos {
            // SAFETY: `lgrui0_name` is a NUL-terminated PWSTR owned by the
            // NetApi buffer and valid until it is freed below.
            if let Ok(name) = unsafe { info.lgrui0_name.to_string() } {
                names.push(name);
            }
        }
    }
    if !buf_ptr.is_null() {
        // SAFETY: the buffer was allocated by NetUserGetLocalGroups.
        let _ = unsafe { NetApiBufferFree(Some(buf_ptr.cast())) };
    }
    Ok(names)
}
