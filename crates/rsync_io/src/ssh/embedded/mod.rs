//! Embedded SSH transport using the russh library.
//!
//! Provides a pure-Rust SSH client as an alternative to spawning the system
//! `ssh` binary. Feature-gated behind `embedded-ssh`. Cipher selection is
//! hardware-aware, preferring AES-GCM on CPUs with AES acceleration.

pub mod cipher;

#[cfg(feature = "embedded-ssh")]
mod config;
#[cfg(feature = "embedded-ssh")]
mod error;
#[cfg(feature = "embedded-ssh")]
mod handler;
#[cfg(feature = "embedded-ssh")]
mod types;

#[cfg(feature = "embedded-ssh")]
pub use config::SshConfig;
#[cfg(feature = "embedded-ssh")]
pub use error::SshError;
#[cfg(feature = "embedded-ssh")]
pub use handler::SshClientHandler;
#[cfg(feature = "embedded-ssh")]
pub use types::{IpPreference, StrictHostKeyChecking};
