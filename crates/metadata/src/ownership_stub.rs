//! No-op ownership helpers for non-Unix platforms.
//!
//! On platforms without POSIX uid/gid semantics, these functions return
//! the raw numeric value unchanged.  This allows callers to use the
//! same API unconditionally without cfg-gating their own code.

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
