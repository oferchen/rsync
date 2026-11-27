//! C ABI type aliases whose widths the `--version` report prints.
//!
//! upstream: `usage.c:48-50` prints `sizeof (OFF_T)`, `sizeof st_ino` and
//! `sizeof (time_t)` as "N-bit files", "N-bit inums" and "N-bit timestamps".

/// The platform `off_t`.
pub type OffT = libc::off_t;

/// The platform `ino_t`.
pub type InoT = libc::ino_t;

/// The platform `time_t`.
///
/// `libc::time_t` is deprecated on musl (musl 1.2.0+ is 64-bit `time_t`),
/// so musl spells it `i64` rather than naming the deprecated alias.
#[cfg(target_env = "musl")]
pub type TimeT = i64;

/// The platform `time_t`.
#[cfg(not(target_env = "musl"))]
pub type TimeT = libc::time_t;
