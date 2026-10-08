#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Shared test utilities for the oc-rsync workspace.
//!
//! This crate provides helpers that multiple test suites need, avoiding
//! duplicated retry logic and setup boilerplate across crates.

pub mod bin_path;
pub mod capabilities;
pub mod clean_fname;
pub mod cli;
pub mod daemon_port;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod daemon_process;
pub mod deadline;
pub mod dir_diff;
pub mod lsh;
#[cfg(feature = "quic")]
pub mod quic_cert;
pub mod reap;
pub mod skip;
pub mod transcript;
#[cfg(unix)]
pub mod umask;
pub mod upstream_compat;

pub use bin_path::{oc_rsync_bin, target_profile_dir, workspace_bin, workspace_bin_path};
pub use capabilities::Capabilities;
pub use clean_fname::COLLAPSE_CASES;
pub use cli::{CliOutput, OcRsyncCliRunner, RunnerError};
pub use daemon_port::{BIND_FAILURE_EXIT_CODE, daemon_listen_port, spawn_daemon_on_free_port};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use daemon_process::{CLIENT_IO_TIMEOUT, DaemonProcess};
pub use deadline::{Deadlined, run_deadlined, run_deadlined_with_stdin};
pub use dir_diff::{DirDiff, DirDiffEntry, DirDiffError, DirDiffMismatch, DirDiffOptions};
pub use lsh::{LSH_STUB_BIN, LshError, LshRunnerStub};
pub use reap::ReapOnDrop;
pub use skip::{
    locate_command_on_path, locate_workspace_binary, require_binary, require_command_on_path,
    require_unix,
};
pub use transcript::{TranscriptError, TranscriptRecorder, WireTranscript};
#[cfg(unix)]
pub use umask::umask_masked;
pub use upstream_compat::{
    UpstreamRsync, UpstreamVersion, locate_upstream_rsync, require_upstream_rsync,
    upstream_compat_enabled, upstream_install_bin, workspace_root,
};

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::{NamedTempFile, TempDir};

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

/// Returns the `.tmp<pid>-` prefix every temp path created here carries.
///
/// nextest runs each test in its own process, and `tempfile` names temp paths
/// with a `fastrand` generator seeded only from the clock and the thread id.
/// The test thread id is the same in every process, so concurrent test
/// processes that seed within one clock tick draw the same names. On Windows,
/// creating a path over another process's same-named directory or
/// delete-pending file fails with `PermissionDenied`, which `tempfile` does not
/// retry (it retries only `AlreadyExists`). The process id makes names from
/// concurrently live processes disjoint.
#[must_use]
pub fn temp_prefix() -> String {
    format!(".tmp{}-", std::process::id())
}

/// Creates a temporary directory whose name is unique across concurrent test
/// processes (see [`temp_prefix`]).
///
/// # Panics
///
/// Panics if the directory cannot be created.
#[must_use]
pub fn create_tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix(&temp_prefix())
        .tempdir()
        .unwrap_or_else(|e| panic!("create tempdir: {e}"))
}

/// Creates a named temporary file whose name is unique across concurrent test
/// processes (see [`temp_prefix`]).
///
/// # Panics
///
/// Panics if the file cannot be created.
#[must_use]
pub fn create_named_tempfile() -> NamedTempFile {
    tempfile::Builder::new()
        .prefix(&temp_prefix())
        .tempfile()
        .unwrap_or_else(|e| panic!("create named tempfile: {e}"))
}

/// Creates a temporary directory (see [`create_tempdir`]) and returns it with
/// its canonical path.
///
/// The system temp dir can sit under a symlink (macOS `/tmp -> /private/tmp`,
/// some CI runners). Confined opens that refuse symlinks in the path, and
/// assertions against resolved paths, need the canonical form. Keep the
/// returned `TempDir` alive for as long as the path is used.
///
/// # Panics
///
/// Panics if the directory cannot be created or canonicalised.
#[must_use]
pub fn create_canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = create_tempdir();
    let canon = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize tempdir: {e}"));
    (dir, canon)
}
