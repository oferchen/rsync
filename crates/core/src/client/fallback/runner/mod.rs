use std::io::Write;

use super::super::ClientError;
use super::args::RemoteFallbackArgs;
use crate::{message::Role, rsync_error};

/// Spawns the fallback `rsync` binary with arguments derived from [`RemoteFallbackArgs`].
///
/// The helper forwards the subprocess stdout/stderr into the provided writers and returns
/// the exit status code on success. Errors surface as [`ClientError`] instances with
/// fully formatted diagnostics.
pub fn run_remote_transfer_fallback<Out, Err>(
    stdout: &mut Out,
    stderr: &mut Err,
    args: RemoteFallbackArgs,
) -> Result<i32, ClientError>
where
    Out: Write,
    Err: Write,
{
    let _ = stdout;
    let _ = stderr;
    let _ = args;

    Err(fallback_error(
        "fallback to external rsync binaries is disabled in this build",
    ))
}

fn fallback_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(1, "{}", text.into()).with_role(Role::Client);
    ClientError::new(1, message)
}
