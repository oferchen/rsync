//! Shared test utilities for the oc-rsync workspace.
//!
//! This crate provides helpers that multiple test suites need, avoiding
//! duplicated retry logic and setup boilerplate across crates.

use std::thread;
use std::time::Duration;

use tempfile::TempDir;

/// Creates a temporary directory with retry logic for transient OS errors.
///
/// Windows CI runners occasionally return `PermissionDenied` from
/// `tempdir()` due to antivirus or filesystem lock contention. This
/// helper retries up to 3 times with exponential backoff (50ms, 100ms,
/// 150ms) before panicking.
#[must_use]
pub fn create_tempdir() -> TempDir {
    const MAX_RETRIES: u32 = 3;
    for attempt in 1..=MAX_RETRIES {
        match tempfile::tempdir() {
            Ok(dir) => return dir,
            Err(e) if attempt < MAX_RETRIES => {
                thread::sleep(Duration::from_millis(50 * u64::from(attempt)));
                eprintln!("tempdir attempt {attempt}/{MAX_RETRIES} failed: {e}");
            }
            Err(e) => panic!("tempdir failed after {MAX_RETRIES} attempts: {e}"),
        }
    }
    unreachable!()
}
