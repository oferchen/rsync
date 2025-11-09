#![allow(unsafe_code)]

#[cfg(unix)]
pub(crate) fn uid_from_raw(raw: rustix::process::RawUid) -> rustix::fs::Uid {
    unsafe { rustix::fs::Uid::from_raw(raw) }
}

#[cfg(unix)]
pub(crate) fn gid_from_raw(raw: rustix::process::RawGid) -> rustix::fs::Gid {
    unsafe { rustix::fs::Gid::from_raw(raw) }
}
