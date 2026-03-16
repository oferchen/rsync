//! crates/logging/src/macros.rs
//! Convenience macros for info and debug logging.

/// Emit an info log message if the flag level is enabled.
///
/// # Example
/// ```ignore
/// info_log!(Copy, 1, "copied {} bytes", bytes);
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
/// # Example
/// ```ignore
/// debug_log!(Recv, 2, "received block offset={}", offset);
/// ```
#[macro_export]
macro_rules! debug_log {
    ($flag:ident, $level:expr, $($arg:tt)*) => {
        if $crate::debug_gte($crate::DebugFlag::$flag, $level) {
            $crate::emit_debug($crate::DebugFlag::$flag, $level, format!($($arg)*));
        }
    };
}
