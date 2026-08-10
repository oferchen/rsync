//! Shared helpers for manipulating environment variables within daemon tests.
//!
//! Delegates to `platform::env::EnvGuard` for the actual unsafe env
//! operations. The daemon crate remains 100% safe Rust.

use std::sync::Mutex;

/// Global mutex guarding environment mutations performed by daemon tests.
///
/// Tests in this crate adjust environment variables such as
/// `OC_RSYNC_FALLBACK` and `RSYNC_PROXY`. Acquiring the mutex before applying
/// overrides ensures the environment remains consistent even when tests run in
/// parallel.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Re-export `platform::env::EnvGuard` for use in daemon tests.
pub(crate) use platform::env::EnvGuard;
