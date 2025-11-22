#![deny(unsafe_code)]

//! Server-side configuration and orchestration helpers.

mod config;
mod role;

pub use config::ServerConfig;
pub use role::ServerRole;
