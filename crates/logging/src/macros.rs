//! Convenience macros for info and debug logging.
//!
//! These macros gate message formatting behind a level check so that
//! disabled messages incur zero allocation cost. They mirror upstream
//! rsync's `DEBUG_GTE(flag, level)` and `INFO_GTE(flag, level)` macros
//! (upstream: rsync.h).

/// Emit an info log message if the flag level is enabled.
///
/// The message is only formatted when the current thread's
/// [`VerbosityConfig`](crate::VerbosityConfig) has the given
/// [`InfoFlag`](crate::InfoFlag) at or above `$level`. This mirrors
/// upstream rsync's `INFO_GTE(flag, level)` guard (upstream: rsync.h).
///
/// # Arguments
///
/// - `$flag` - An [`InfoFlag`](crate::InfoFlag) variant name (e.g., `Copy`, `Del`, `Name`).
/// - `$level` - Minimum level (`u8`). Level 1 corresponds to `-v`, level 2 to `-vv`.
/// - `$($arg)*` - Format arguments, passed to `format!()`.
///
/// # Examples
///
/// ```rust,ignore
/// // Emitted at -v (level 1)
/// info_log!(Name, 1, "transferring {}", path);
///
/// // Emitted at -vv (level 2) - shows skipped files
/// info_log!(Skip, 1, "skipping non-regular file: {}", path);
/// ```
#[macro_export]
macro_rules! info_log {
    ($flag:ident, $level:expr, $($arg:tt)*) => {
        if $crate::info_gte($crate::InfoFlag::$flag, $level) {
            $crate::emit_info($crate::InfoFlag::$flag, $level, format!($($arg)*));
        }
    };
}

/// Emit a debug log message if the flag level is enabled.
///
/// The message is only formatted when the current thread's
/// [`VerbosityConfig`](crate::VerbosityConfig) has the given
/// [`DebugFlag`](crate::DebugFlag) at or above `$level`. This mirrors
/// upstream rsync's `DEBUG_GTE(flag, level)` guard (upstream: rsync.h).
///
/// Debug flags are activated starting at `-vv` (verbose level 2). Higher
/// verbose levels increase individual flag levels, enabling progressively
/// more detailed output. See [`VerbosityConfig::from_verbose_level`] for
/// the full mapping.
///
/// # Arguments
///
/// - `$flag` - A [`DebugFlag`](crate::DebugFlag) variant name (e.g., `Deltasum`, `Recv`, `Io`).
/// - `$level` - Minimum level (`u8`). Higher levels produce more detailed output.
/// - `$($arg)*` - Format arguments, passed to `format!()`.
///
/// # Examples
///
/// ```rust,ignore
/// // Emitted at -vv (deltasum level 1)
/// debug_log!(Deltasum, 1, "sum count={} block_len={}", count, block_len);
///
/// // Emitted at -vvv (deltasum level 2) - per-block detail
/// debug_log!(Deltasum, 2, "block {} offset={} matched", idx, offset);
///
/// // Emitted at -vvvv (deltasum level 3) - hash detail
/// debug_log!(Deltasum, 3, "hash search s1={:#x} s2={:#x}", s1, s2);
/// ```
#[macro_export]
macro_rules! debug_log {
    ($flag:ident, $level:expr, $($arg:tt)*) => {
        if $crate::debug_gte($crate::DebugFlag::$flag, $level) {
            $crate::emit_debug($crate::DebugFlag::$flag, $level, format!($($arg)*));
        }
    };
}
