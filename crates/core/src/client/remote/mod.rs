//! Remote transfer orchestration for SSH and daemon transports.
//!
//! This module provides the infrastructure for executing rsync transfers over
//! remote connections, including SSH and rsync daemon protocols.

pub mod daemon_transfer;
pub mod invocation;
pub mod ssh_transfer;

pub use daemon_transfer::run_daemon_transfer;
pub use invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, determine_transfer_role, operand_is_remote,
};
pub use ssh_transfer::run_ssh_transfer;
