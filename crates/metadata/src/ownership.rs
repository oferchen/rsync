#![allow(unsafe_code)]

//! Raw UID/GID conversion helpers.
//!
//! Provides safe wrappers around rustix's `from_raw` methods for
//! converting raw platform identifiers to typed wrappers.

#[cfg(unix)]
pub(crate) const fn uid_from_raw(raw: rustix::process::RawUid) -> rustix::fs::Uid {
    // SAFETY: `Uid::from_raw` creates a typed wrapper around the raw UID value.
    // Any u32 value is valid as a UID on Unix systems (including -1/u32::MAX
    // which represents "no change" in chown operations).
    unsafe { rustix::fs::Uid::from_raw(raw) }
}

#[cfg(unix)]
pub(crate) const fn gid_from_raw(raw: rustix::process::RawGid) -> rustix::fs::Gid {
    // SAFETY: `Gid::from_raw` creates a typed wrapper around the raw GID value.
    // Any u32 value is valid as a GID on Unix systems (including -1/u32::MAX
    // which represents "no change" in chown operations).
    unsafe { rustix::fs::Gid::from_raw(raw) }
}
