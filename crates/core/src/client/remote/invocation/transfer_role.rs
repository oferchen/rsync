//! Transfer role detection and remote operand parsing.
//!
//! Analyzes source and destination operands to determine whether a transfer is
//! push (local -> remote), pull (remote -> local), or proxy (remote -> remote).
//! This mirrors the operand analysis in upstream `main.c:start_client()` which
//! determines the local process role based on which operands are remote.
//!
//! # Upstream Reference
//!
//! - `main.c:start_client()` - Role determination from operand positions
//! - `options.c:check_for_hostspec()` - Remote operand detection

use std::ffi::OsString;

use super::super::super::error::{
    ClientError, cause_line_error, cause_lines_error, invalid_argument_error,
};
use super::super::hostspec::{check_for_hostspec, operand_bytes};
use super::{RemoteOperands, TransferSpec};
use crate::exit_code::ExitCode;
use crate::message::Role;

/// Checks if an operand represents a remote path.
///
/// Re-exported from [`engine::operand`], the single source of truth mirroring
/// upstream `options.c:check_for_hostspec()`. Sharing one classifier keeps this
/// remote-invocation planner, the CLI front end, and the local-copy executor
/// from ever disagreeing on whether an operand is local, remote, or a daemon
/// module; a divergent verdict would ship the wrong operand to the remote server.
pub use engine::operand::operand_is_remote;

/// Checks every further source of a remote pull against the first, as
/// upstream does before connecting.
///
/// `option_port` is the `--port` value (0 when unset): upstream's `rsync_port`
/// starts there, so a daemon spelling without its own port keeps it.
///
/// upstream: main.c:1480 parses the first source into `shell_machine` and
/// `rsync_port`, and main.c:1586-1610 requires each later one to be a hostspec
/// (`Unexpected local arg`), to name the same host or none, and to use the
/// same hostspec format and port. `dummy_port` is carried from one source to
/// the next, so a source without an explicit port inherits the previous one.
fn validate_remote_sources(sources: &[OsString], option_port: i32) -> Result<(), ClientError> {
    let mut rsync_port = option_port;
    let first = operand_bytes(&sources[0]);
    let Some(first) = check_for_hostspec(first, Some(&mut rsync_port)) else {
        return Err(unexpected_local_arg(&sources[0]));
    };
    let mut dummy_port = rsync_port;
    for source in &sources[1..] {
        let Some(spec) = check_for_hostspec(operand_bytes(source), Some(&mut dummy_port)) else {
            return Err(unexpected_local_arg(source));
        };
        if !spec.host.is_empty() && spec.host != first.host {
            return Err(syntax_refusal(
                "All source args must come from the same machine.",
                Role::PreForkReceiver,
            ));
        }
        if rsync_port != dummy_port {
            let message = if rsync_port == 0 || dummy_port == 0 {
                "All source args must use the same hostspec format."
            } else {
                "All source args must use the same port number."
            };
            return Err(syntax_refusal(message, Role::PreForkReceiver));
        }
    }
    Ok(())
}

/// upstream: main.c:1591-1592 - two `rprintf(FERROR, ...)` calls, so two
/// lines; a newline in the operand itself is escaped as `\#012`.
fn unexpected_local_arg(source: &OsString) -> ClientError {
    cause_lines_error(
        vec![
            format!("Unexpected local arg: {}", source.to_string_lossy()),
            "If arg is a remote file/dir, prefix it with a colon (:).".to_owned(),
        ],
        ExitCode::Syntax,
        Role::PreForkReceiver,
    )
}

/// A source-list refusal as upstream prints it: the `rprintf(FERROR, ...)`
/// text on its own lines, then `exit_cleanup(RERR_SYNTAX)`'s trailer tagged
/// with `who_am_i()` - `Receiver` before a pull forks, `sender` for a push.
fn syntax_refusal(text: &str, role: Role) -> ClientError {
    cause_line_error(text.to_owned(), ExitCode::Syntax, role)
}

/// Determines the transfer type and role from source and destination operands.
///
/// Analyzes the operands to determine whether this is a push (local -> remote)
/// or pull (remote -> local) transfer. A pull's sources are checked with no
/// `--port` value; `determine_transfer_role_with_port` takes one.
///
/// # Errors
///
/// Returns error if:
/// - Neither source nor destination is remote (should use local copy)
/// - Both a source and the destination are remote
/// - A local source follows a remote one, or a remote one follows a local one
/// - Remote sources name different hosts, hostspec formats, or ports
pub fn determine_transfer_role(
    sources: &[OsString],
    destination: &OsString,
) -> Result<TransferSpec, ClientError> {
    determine_transfer_role_with_port(sources, destination, None)
}

/// [`determine_transfer_role`] with the `--port` value a pull's sources are
/// checked against.
///
/// upstream: main.c:1474-1610 - the first source decides the direction. A
/// remote first source makes a pull, and every later source must match it
/// ([`validate_remote_sources`]); a local first source makes a push (or a
/// local copy), and every later source must be local (main.c:1569-1578).
///
/// # Errors
///
/// As [`determine_transfer_role`].
pub(crate) fn determine_transfer_role_with_port(
    sources: &[OsString],
    destination: &OsString,
    option_port: Option<u16>,
) -> Result<TransferSpec, ClientError> {
    let dest_is_remote = operand_is_remote(destination);

    if !sources
        .first()
        .is_some_and(|first| operand_is_remote(first))
    {
        if let Some(remote) = sources.iter().skip(1).find(|s| operand_is_remote(s)) {
            // upstream: main.c:1575-1577.
            return Err(syntax_refusal(
                &format!("Unexpected remote arg: {}", remote.to_string_lossy()),
                Role::Sender,
            ));
        }
        if !dest_is_remote {
            return Err(invalid_argument_error("no remote operand found", 1));
        }
        return Ok(TransferSpec::Push {
            local_sources: sources.to_vec(),
            remote_dest: destination.clone(),
        });
    }

    // upstream: main.c:1490-1493 refuses before any connection is made.
    if dest_is_remote {
        return Err(invalid_argument_error(
            "The source and destination cannot both be remote.",
            1,
        ));
    }

    validate_remote_sources(sources, option_port.map_or(0, i32::from))?;

    // Preserve the operand bytes verbatim (a non-UTF-8 remote path is a legal
    // Unix filename).
    let remote_sources = if sources.len() > 1 {
        RemoteOperands::Multiple(sources.to_vec())
    } else {
        RemoteOperands::Single(sources[0].clone())
    };

    Ok(TransferSpec::Pull {
        remote_sources,
        local_dest: destination.clone(),
    })
}
