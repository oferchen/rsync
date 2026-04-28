//! Raw UID/GID conversion helpers (Unix).
//!
//! Mirrors the surface of [`ownership_stub`] so callers can use one API on every
//! platform: `ownership::uid_from_raw` / `ownership::gid_from_raw`. On Unix these
//! produce typed [`rustix::fs::Uid`] / [`rustix::fs::Gid`] values; the non-Unix
//! stub is the identity over `u32`.
//!
//! [`ownership_stub`]: crate::ownership_stub

#[inline]
pub(crate) fn uid_from_raw(raw: rustix::process::RawUid) -> rustix::fs::Uid {
    rustix::fs::Uid::from_raw(raw)
}

#[inline]
pub(crate) fn gid_from_raw(raw: rustix::process::RawGid) -> rustix::fs::Gid {
    rustix::fs::Gid::from_raw(raw)
}
