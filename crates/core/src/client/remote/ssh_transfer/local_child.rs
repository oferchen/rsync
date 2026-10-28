//! Local copy over a spawned `--server` child.
//!
//! Upstream runs every local copy as a sender parent plus a forked receiver
//! and generator child (main.c:666-667 `local_child()`). oc-rsync keeps its
//! in-process engine for local copies and takes this path only when a role
//! trace is requested, so the trace comes from the same per-role code that
//! emits it on a remote transfer.

use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use engine::batch::BatchWriter;
use rsync_io::ssh::SshCommand;

use super::super::super::config::ClientConfig;
use super::super::super::error::{ClientError, IPC_EXIT_CODE, invalid_argument_error};
use super::super::super::progress::ClientProgressObserver;
use super::super::super::summary::ClientSummary;
use super::super::invocation::{RemoteInvocationBuilder, RemoteRole};
use super::drive::run_push_transfer;

/// Runs a local copy as a push to an `oc-rsync --server` child of this binary.
///
/// The parent is the sender, as upstream asserts (pipe.c:116); the child
/// receives over a socketpair, like upstream's `fd_pair()` (pipe.c:119).
///
/// # Errors
///
/// Returns an IPC error (exit 14) when the running executable cannot be
/// resolved or the child cannot be spawned, and otherwise any error the push
/// transfer reports.
pub fn run_local_child_transfer(
    config: &ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
) -> Result<ClientSummary, ClientError> {
    let config = config.for_local_child();
    let Some((destination, sources)) = config.transfer_args().split_last() else {
        return Err(invalid_argument_error(
            "a local copy needs a source and a destination",
            1,
        ));
    };

    let program = std::env::current_exe().map_err(|e| {
        invalid_argument_error(
            &format!("failed to resolve the local server program: {e}"),
            IPC_EXIT_CODE,
        )
    })?;
    let args = RemoteInvocationBuilder::new(&config, RemoteRole::Sender)
        .build_local_child(destination);

    let mut command = SshCommand::new(OsString::new());
    command
        .set_program(program)
        .set_local_child(true)
        .set_connect_timeout(None)
        .set_io_timeout(config.ssh_io_timeout())
        .set_remote_command(args);
    let connection = command.spawn().map_err(|e| {
        invalid_argument_error(&format!("fork: {e}"), IPC_EXIT_CODE)
    })?;

    run_push_transfer(&config, connection, sources, observer, batch_writer)
}
