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
//! Uses the `windows` crate (v0.61) for Win32 API bindings. Unsafe blocks
//! are required for FFI calls but are scoped to individual functions.
//!
//! # Safety Policy
//!
//! This crate uses `#![deny(unsafe_code)]` at the crate level. Individual
//! functions that require unsafe are annotated with `#[allow(unsafe_code)]`
//! and include detailed `// SAFETY:` comments.

#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod daemonize;
pub mod env;
pub mod group;
pub mod name_resolution;
pub mod privilege;
pub mod secrets;
pub mod signal;
pub mod windows_service;
