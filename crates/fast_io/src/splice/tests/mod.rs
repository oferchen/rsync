//! Tests for the `splice` module, grouped by target platform.

use super::*;

#[test]
fn test_is_splice_available_returns_bool() {
    // On any platform, this should return a boolean without panicking.
    let _available = is_splice_available();
}

#[cfg(not(target_os = "linux"))]
mod non_linux;

#[cfg(target_os = "linux")]
mod linux;
