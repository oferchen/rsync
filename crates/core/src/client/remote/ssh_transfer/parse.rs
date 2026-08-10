//! Remote operand parsing and rsync invocation argument construction.
//!
//! Parses `host:path` / `ssh://` operands into connection details and builds
//! the remote `--server` invocation argument list (upstream:
//! `options.c:server_options()`).

use std::ffi::{OsStr, OsString};

use rsync_io::ssh::parse_ssh_operand;

use super::super::super::config::ClientConfig;
use super::super::super::error::{ClientError, invalid_argument_error};
use super::super::invocation::{RemoteInvocationBuilder, RemoteOperands, RemoteRole};

/// SSH invocation result containing args, host, optional user, optional port, and stdin args.
///
/// Used by `parse_single_remote` and `parse_remote_operands` to return parsed
/// remote connection information along with the rsync invocation arguments.
/// The final `Vec<String>` contains arguments to send over stdin when
/// secluded-args is active (empty when disabled).
pub(super) type SshInvocationResult = (
    Vec<OsString>,
    String,
    Option<String>,
    Option<u16>,
    Vec<String>,
);

/// Parses a single remote operand and builds the invocation args.
pub(in crate::client::remote) fn parse_single_remote(
    operand_str: &str,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<SshInvocationResult, ClientError> {
    let operand = parse_ssh_operand(OsStr::new(operand_str))
        .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

    let invocation_builder = RemoteInvocationBuilder::new(config, role);
    let secluded = invocation_builder.build_secluded(&[operand.path()]);

    Ok((
        secluded.command_line_args,
        operand.host().to_owned(),
        operand.user().map(String::from),
        operand.port(),
        secluded.stdin_args,
    ))
}

/// Parses remote operand(s) and builds the invocation args.
pub(in crate::client::remote) fn parse_remote_operands(
    remote_operands: &RemoteOperands,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<SshInvocationResult, ClientError> {
    match remote_operands {
        RemoteOperands::Single(operand_str) => parse_single_remote(operand_str, config, role),
        RemoteOperands::Multiple(operand_strs) => {
            let first_operand = parse_ssh_operand(OsStr::new(&operand_strs[0]))
                .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

            let mut paths = Vec::new();
            for operand_str in operand_strs {
                let operand = parse_ssh_operand(OsStr::new(operand_str)).map_err(|e| {
                    invalid_argument_error(&format!("invalid remote operand: {e}"), 1)
                })?;
                paths.push(operand.path().to_owned());
            }

            let invocation_builder = RemoteInvocationBuilder::new(config, role);
            let path_refs: Vec<&str> = paths.iter().map(|s| s.as_ref()).collect();
            let secluded = invocation_builder.build_secluded(&path_refs);

            Ok((
                secluded.command_line_args,
                first_operand.host().to_owned(),
                first_operand.user().map(String::from),
                first_operand.port(),
                secluded.stdin_args,
            ))
        }
    }
}
