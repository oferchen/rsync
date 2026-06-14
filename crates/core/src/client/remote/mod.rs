//! Remote transfer orchestration for SSH and daemon transports.
//!
//! This module provides the infrastructure for executing rsync transfers over
//! remote connections, including SSH and rsync daemon protocols. It mirrors
//! the dispatch paths in upstream `main.c:do_cmd()` (SSH) and
//! `clientserver.c:start_daemon_client()` (daemon).
//!
//! # Submodules
//!
//! - `daemon_transfer` - Daemon protocol (rsync://) connection, handshake, and transfer
//! - `ssh_transfer` - SSH-based remote transfers via `--rsh`/`-e`
//! - `invocation` - Remote rsync `--server` argument construction and role detection
//! - `flags` - Shared flag builder functions for compact server option strings
//! - `remote_to_remote` - Two-host proxy relay via local machine
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - SSH command spawning
//! - `main.c:start_server()` - Server-side entry after SSH
//! - `clientserver.c:start_daemon_client()` - Daemon URL dispatch
//! - `options.c:server_options()` - Server flag string generation

#[cfg(feature = "async-ssh")]
pub mod async_ssh_transport;
pub(crate) mod batch_support;
/// Daemon transfer orchestration for `rsync://` URLs.
pub mod daemon_transfer;
/// Embedded SSH transfer orchestration using the russh library.
#[cfg(feature = "embedded-ssh")]
pub mod embedded_ssh_transfer;
pub(crate) mod flags;
/// Remote rsync `--server` invocation argument builder.
pub mod invocation;
/// Remote-to-remote transfer via local proxy relay.
pub mod remote_to_remote;
/// SSH transfer orchestration for `ssh://` and `host:path` targets.
pub mod ssh_transfer;
#[cfg(feature = "async-ssh")]
pub use async_ssh_transport::{
    ENV_OPT_IN as ASYNC_SSH_ENV_OPT_IN, is_enabled_by_env as async_ssh_enabled,
    run_async_ssh_transfer,
};
pub use daemon_transfer::{run_daemon_over_remote_shell, run_daemon_transfer};
#[cfg(feature = "embedded-ssh")]
pub(crate) use embedded_ssh_transfer::is_ssh_url;
#[cfg(feature = "embedded-ssh")]
pub use embedded_ssh_transfer::run_embedded_ssh_transfer;
pub use invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, SecludedInvocation, TransferSpec,
    determine_transfer_role, operand_is_remote,
};
pub use ssh_transfer::run_ssh_transfer;
