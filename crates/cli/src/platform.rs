//! Platform-specific helpers for user and group identity lookups.

#[cfg(unix)]
use users::{
    get_group_by_gid, get_group_by_name, get_user_by_name, get_user_by_uid, gid_t as UsersGid,
    uid_t as UsersUid,
};

#[cfg(unix)]
#[allow(non_camel_case_types)]
pub(crate) type gid_t = UsersGid;
#[cfg(unix)]
#[allow(non_camel_case_types)]
pub(crate) type uid_t = UsersUid;

#[cfg(not(unix))]
#[allow(non_camel_case_types)]
pub(crate) type gid_t = u32;
#[cfg(not(unix))]
#[allow(non_camel_case_types)]
pub(crate) type uid_t = u32;

/// Indicates whether the platform supports resolving user names.
pub(crate) const SUPPORTS_USER_NAME_LOOKUP: bool = cfg!(unix);
/// Indicates whether the platform supports resolving group names.
pub(crate) const SUPPORTS_GROUP_NAME_LOOKUP: bool = cfg!(unix);

#[cfg(unix)]
fn to_owned<C>(value: C) -> Option<String>
where
    C: AsRef<std::ffi::OsStr>,
{
    let value = value.as_ref();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string_lossy().into_owned())
    }
}

#[cfg(unix)]
#[inline]
pub(crate) fn lookup_user_by_name(name: &str) -> Option<uid_t> {
    get_user_by_name(name).map(|user| user.uid())
}

#[cfg(not(unix))]
#[inline]
pub(crate) fn lookup_user_by_name(_name: &str) -> Option<uid_t> {
    None
}

#[cfg(unix)]
#[inline]
pub(crate) fn lookup_group_by_name(name: &str) -> Option<gid_t> {
    get_group_by_name(name).map(|group| group.gid())
}

#[cfg(not(unix))]
#[inline]
pub(crate) fn lookup_group_by_name(_name: &str) -> Option<gid_t> {
    None
}

#[cfg(unix)]
#[inline]
pub(crate) fn display_user_name(uid: u32) -> Option<String> {
    get_user_by_uid(uid as uid_t).and_then(|user| to_owned(user.name()))
}

#[cfg(not(unix))]
#[inline]
pub(crate) fn display_user_name(_uid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
#[inline]
pub(crate) fn display_group_name(gid: u32) -> Option<String> {
    get_group_by_gid(gid as gid_t).and_then(|group| to_owned(group.name()))
}

#[cfg(not(unix))]
#[inline]
pub(crate) fn display_group_name(_gid: u32) -> Option<String> {
    None
}
