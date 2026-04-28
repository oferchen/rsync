//! Raw UID/GID conversion helpers.
//!
//! Provides safe wrappers around rustix's `from_raw` methods for
//! converting raw platform identifiers to typed wrappers.

/// Wraps a raw UID value into a typed `Uid` for use with rustix filesystem operations.
#[cfg(unix)]
pub(crate) fn uid_from_raw(raw: rustix::process::RawUid) -> rustix::fs::Uid {
    rustix::fs::Uid::from_raw(raw)
}

/// Wraps a raw GID value into a typed `Gid` for use with rustix filesystem operations.
#[cfg(unix)]
pub(crate) fn gid_from_raw(raw: rustix::process::RawGid) -> rustix::fs::Gid {
    rustix::fs::Gid::from_raw(raw)
}
