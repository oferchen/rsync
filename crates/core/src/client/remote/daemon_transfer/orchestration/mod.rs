//! Transfer orchestration, argument building, and execution.
//!
//! Builds the daemon argument list (mirroring upstream `server_options()` in
//! `options.c`), configures the server infrastructure for pull/push transfers,
//! and converts server statistics to client summaries.
//!
//! Split into focused submodules by responsibility:
//! - [`arguments`] - daemon argument list construction (single-phase and protect-args)
//! - [`server_config`] - `ServerConfig` builders for receiver and generator roles
//! - [`stats`] - server statistics to client summary conversion
//! - [`transfer`] - pull and push transfer execution

mod arguments;
mod server_config;
mod stats;
mod transfer;

pub(crate) use arguments::send_daemon_arguments;
pub(crate) use transfer::{run_pull_transfer, run_push_transfer};

#[cfg(test)]
mod tests;
