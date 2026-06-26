//! Platform-specific unsafe code isolation for oc-rsync.
//!
//! This crate encapsulates all platform FFI (libc, Windows API) behind safe
//! public APIs. Higher-level crates (daemon, cli, core) depend on this crate
//! and remain 100% safe Rust.
//!
//! # Unix
//!
//! Uses `nix` for safe POSIX wrappers where available (chroot, setuid, setgid,
//! setsid, getuid, dup2, close, open). Falls back to `libc` for operations
//! not covered by nix (setgroups on macOS, fork, getgrnam_r).
//!
//! # Windows
//!
//! Uses the `windows` crate for Win32 API bindings. Unsafe blocks are required
//! for FFI calls but are scoped to individual functions.
//!
//! # Safety Policy
//!
//! This crate uses `#![deny(unsafe_code)]` at the crate level. Individual
//! functions that require unsafe are annotated with `#[allow(unsafe_code)]`
//! and include detailed `// SAFETY:` comments.

#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

/// Process daemonization - fork, setsid, and stdio redirection.
pub mod daemonize;
/// Environment variable manipulation with RAII restoration.
pub mod env;
/// Typed platform error variants used by daemon/cli/core for I/O failures.
pub mod error;
/// System group membership lookups.
pub mod group;
/// Per-instant local timezone offset (mirrors upstream `timestring()`'s
/// `localtime_r`), used to render file modtimes in local time.
pub mod local_time;
/// Windows account name to RID resolution.
pub mod name_resolution;
/// Process privilege operations - chroot and uid/gid dropping.
pub mod privilege;
/// Secrets file permission validation.
pub mod secrets;
/// Signal handler registration and shared atomic flags.
pub mod signal;
/// Windows Service Control Manager (SCM) integration.
pub mod windows_service;
