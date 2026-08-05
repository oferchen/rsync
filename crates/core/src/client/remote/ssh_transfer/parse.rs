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
/// The final `Vec<OsString>` contains arguments to send over stdin when
/// secluded-args is active (empty when disabled); it is `OsString` so a
/// non-UTF-8 remote path ships verbatim.
pub(super) type SshInvocationResult = (
    Vec<OsString>,
    String,
    Option<String>,
    Option<u16>,
    Vec<OsString>,
);

/// Parses a single remote operand and builds the invocation args.
pub(in crate::client::remote) fn parse_single_remote(
    operand: &OsStr,
    config: &ClientConfig,
    role: RemoteRole,
) -> Result<SshInvocationResult, ClientError> {
    let operand = parse_ssh_operand(operand)
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
        RemoteOperands::Single(operand) => parse_single_remote(operand, config, role),
        RemoteOperands::Multiple(operands) => {
            let first_operand = parse_ssh_operand(&operands[0])
                .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))?;

            let mut paths: Vec<OsString> = Vec::new();
            for operand in operands {
                let parsed = parse_ssh_operand(operand).map_err(|e| {
                    invalid_argument_error(&format!("invalid remote operand: {e}"), 1)
                })?;
                paths.push(parsed.path().to_os_string());
            }

            let invocation_builder = RemoteInvocationBuilder::new(config, role);
            let path_refs: Vec<&OsStr> = paths.iter().map(OsString::as_os_str).collect();
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

/// Extracts the host-stripped path portion of each remote pull source operand.
///
/// These paths are recorded as implied includes so the receiver can reject any
/// file-list name the remote sender was never asked for (CVE-2022-29154).
/// Mirrors upstream `check_for_hostspec()`, which returns the operand's path
/// portion before `add_implied_include()` records it (main.c:1525,1549).
pub(in crate::client::remote) fn remote_operand_source_paths(
    operands: &RemoteOperands,
) -> Result<Vec<String>, ClientError> {
    let operand_list: &[OsString] = match operands {
        RemoteOperands::Single(operand) => std::slice::from_ref(operand),
        RemoteOperands::Multiple(operands) => operands.as_slice(),
    };
    // These feed the receiver-side implied-include check (CVE-2022-29154),
    // which matches against the `String`-typed filter machinery; a lossy view
    // is intentional here and independent of the byte-faithful operand that
    // `parse_remote_operands` ships to the remote sender.
    operand_list
        .iter()
        .map(|operand| {
            parse_ssh_operand(operand)
                .map(|parsed| parsed.path().to_string_lossy().into_owned())
                .map_err(|e| invalid_argument_error(&format!("invalid remote operand: {e}"), 1))
        })
        .collect()
}
