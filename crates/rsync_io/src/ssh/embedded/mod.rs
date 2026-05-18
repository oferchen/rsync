//! Embedded SSH transport using the russh library.
//!
//! Provides a pure-Rust SSH client as an alternative to spawning the system
//! `ssh` binary. Feature-gated behind `embedded-ssh`. Cipher selection is
//! hardware-aware, preferring AES-GCM on CPUs with AES acceleration.

pub mod cipher;

#[cfg(feature = "embedded-ssh")]
mod auth;
#[cfg(feature = "embedded-ssh")]
mod config;
#[cfg(feature = "embedded-ssh")]
mod connect;
#[cfg(feature = "embedded-ssh")]
mod error;
#[cfg(feature = "embedded-ssh")]
mod handler;
#[cfg(feature = "embedded-ssh")]
mod resolve;
#[cfg(feature = "embedded-ssh")]
mod ssh_config;
/// Sync/async bridge primitives for embedded SSH streams.
#[cfg(feature = "embedded-ssh")]
pub mod sync_bridge;
#[cfg(feature = "embedded-ssh")]
mod types;

#[cfg(feature = "embedded-ssh")]
pub use auth::authenticate;
#[cfg(feature = "embedded-ssh")]
pub use config::SshConfig;
#[cfg(feature = "embedded-ssh")]
pub use connect::{ChannelReader, ChannelWriter, connect_and_exec};
#[cfg(feature = "embedded-ssh")]
pub use error::SshError;
#[cfg(feature = "embedded-ssh")]
pub use handler::SshClientHandler;
#[cfg(feature = "embedded-ssh")]
pub use resolve::resolve_host;
#[cfg(feature = "embedded-ssh")]
pub use sync_bridge::{
    DEFAULT_CHANNEL_CAPACITY, SyncAsyncBridge, SyncReader as BridgeSyncReader,
    SyncWriter as BridgeSyncWriter, into_sync_halves, into_sync_halves_with_capacity,
};
#[cfg(feature = "embedded-ssh")]
pub use types::{IpPreference, StrictHostKeyChecking};
