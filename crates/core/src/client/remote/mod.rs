//! Remote transfer orchestration for SSH and daemon transports.
//!
//! This module provides the infrastructure for executing rsync transfers over
//! remote connections, including SSH and rsync daemon protocols. It mirrors
//! the dispatch paths in upstream `main.c:do_cmd()` (SSH) and
//! `clientserver.c:start_daemon_client()` (daemon).
//!
//! # Submodules
//!
//! - [`daemon_transfer`] - Daemon protocol (rsync://) connection, handshake, and transfer
//! - [`ssh_transfer`] - SSH-based remote transfers via `--rsh`/`-e`
//! - [`invocation`] - Remote rsync `--server` argument construction and role detection
//! - `flags` - Shared flag builder functions for compact server option strings
//! - [`remote_to_remote`] - Two-host proxy relay via local machine
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - SSH command spawning
//! - `main.c:start_server()` - Server-side entry after SSH
//! - `clientserver.c:start_daemon_client()` - Daemon URL dispatch
//! - `options.c:server_options()` - Server flag string generation

pub mod daemon_transfer;
pub(crate) mod flags;
pub mod invocation;
pub mod remote_to_remote;
pub mod ssh_transfer;
pub use daemon_transfer::run_daemon_transfer;
pub use invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, SecludedInvocation, TransferSpec,
    determine_transfer_role, operand_is_remote,
};
pub use ssh_transfer::run_ssh_transfer;
