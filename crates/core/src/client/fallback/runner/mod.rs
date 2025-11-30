use std::io::Write;

use crate::{client::ClientError, message::Role, rsync_error};

use super::args::RemoteFallbackArgs;

fn fallback_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(1, "{}", text.into()).with_role(Role::Client);
    ClientError::new(1, message)
}

/// Formerly spawned a fallback `rsync` binary with arguments derived from
/// [`RemoteFallbackArgs`].
///
/// Delegation to an external `rsync` is no longer permitted; callers receive a
/// branded error indicating that native oc-rsync paths must be used instead.
pub fn run_remote_transfer_fallback<Out, Err>(
    stdout: &mut Out,
    stderr: &mut Err,
    args: RemoteFallbackArgs,
) -> Result<i32, ClientError>
where
    Out: Write,
    Err: Write,
{
    let _ = (stdout, stderr, args);
    Err(fallback_error(
        "system rsync fallback is disabled; native oc-rsync handles all roles",
    ))
}
