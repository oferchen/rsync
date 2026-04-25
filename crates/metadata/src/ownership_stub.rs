//! No-op ownership helpers for non-Unix platforms.
//!
//! On platforms without POSIX uid/gid semantics, these functions return
//! the raw numeric value unchanged.  This allows callers to use the
//! same API unconditionally without cfg-gating their own code.

#![allow(dead_code)]

/// Convert a raw UID to the platform owner type.
///
/// On non-Unix platforms, this is an identity function returning the raw u32.
pub const fn uid_from_raw(raw: u32) -> u32 {
    raw
}

/// Convert a raw GID to the platform group type.
///
/// On non-Unix platforms, this is an identity function returning the raw u32.
pub const fn gid_from_raw(raw: u32) -> u32 {
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_from_raw_identity() {
        assert_eq!(uid_from_raw(0), 0);
        assert_eq!(uid_from_raw(1000), 1000);
        assert_eq!(uid_from_raw(u32::MAX), u32::MAX);
    }

    #[test]
    fn gid_from_raw_identity() {
        assert_eq!(gid_from_raw(0), 0);
        assert_eq!(gid_from_raw(1000), 1000);
        assert_eq!(gid_from_raw(u32::MAX), u32::MAX);
    }
}
