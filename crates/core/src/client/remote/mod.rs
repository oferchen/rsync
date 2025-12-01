//! Remote transfer orchestration for SSH transport.
//!
//! This module provides the infrastructure for executing rsync transfers over
//! SSH connections, including operand parsing, server invocation building, and
//! protocol negotiation.

pub mod invocation;
pub mod ssh_transfer;

pub use invocation::{
    RemoteInvocationBuilder, RemoteRole, determine_transfer_role, operand_is_remote,
};
pub use ssh_transfer::run_ssh_transfer;
