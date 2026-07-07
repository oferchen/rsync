#![deny(rustdoc::broken_intra_doc_links)]

//! Shared test utilities for the oc-rsync workspace.
//!
//! This crate provides helpers that multiple test suites need, avoiding
//! duplicated retry logic and setup boilerplate across crates.

pub mod cli;
pub mod dir_diff;
pub mod lsh;
pub mod skip;
pub mod upstream_compat;

pub use cli::{CliOutput, OcRsyncCliRunner, RunnerError};
pub use dir_diff::{DirDiff, DirDiffEntry, DirDiffError, DirDiffMismatch, DirDiffOptions};
pub use lsh::{LSH_STUB_BIN, LshError, LshRunnerStub};
pub use skip::{
    locate_command_on_path, locate_workspace_binary, require_binary, require_command_on_path,
    require_unix,
};
pub use upstream_compat::{
    UpstreamRsync, UpstreamVersion, locate_upstream_rsync, require_upstream_rsync,
    upstream_compat_enabled,
};

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

/// Process-global mutex serializing tests that touch the shared
/// `engine::CleanupManager` registry (a `OnceLock<Mutex<HashSet>>` singleton).
///
/// Because the registry is process-global and tests call `reset_for_testing`
/// / `register_temp_file` on it, concurrent tests otherwise stomp on each
/// other's state. Acquiring this guard for the duration of such a test
/// serializes them without hiding any production race: the registry itself is
/// mutex-protected and thread-safe; only the test-side `reset`/count
/// expectations are order-sensitive.
fn cleanup_registry_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Acquires the process-global cleanup-registry test lock, serializing any
/// test that mutates the shared `engine::CleanupManager`.
///
/// Hold the returned guard for the whole test body. The lock is poison-tolerant
/// so a panicking test does not deadlock the rest of the suite.
pub fn cleanup_registry_test_guard() -> MutexGuard<'static, ()> {
    cleanup_registry_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
