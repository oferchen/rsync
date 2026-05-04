//! Transfer role detection and remote operand parsing.
//!
//! Analyzes source and destination operands to determine whether a transfer is
//! push (local -> remote), pull (remote -> local), or proxy (remote -> remote).
//! This mirrors the operand analysis in upstream `main.c:do_cmd()` which
//! determines the local process role based on which operands are remote.
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - Role determination from operand positions
//! - `main.c:check_for_hostspec()` - Remote operand detection

use std::ffi::OsStr;
use std::ffi::OsString;

use super::super::super::error::{ClientError, invalid_argument_error};
use super::{RemoteOperandParsed, RemoteOperands, TransferSpec};

/// Checks if an operand represents a remote path.
///
/// Detects `rsync://` URLs, `ssh://` URLs, double-colon daemon syntax
/// (`host::module`), and single-colon SSH syntax (`host:path`). This mirrors upstream
/// `main.c:check_for_hostspec()`. A simplified version that matches the logic in
/// `engine::local_copy::operand_is_remote` which is not public.
pub fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") || text.starts_with("ssh://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        #[cfg(windows)]
        if colon_index == 1
            && text
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_alphabetic())
        {
            return false;
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        return true;
    }

    false
}

/// Parses a remote operand string into its components for validation.
///
/// Handles formats like:
/// - `host:path`
/// - `user@host:path`
/// - `user@host.example.com:path`
/// - `user@[::1]:path` (IPv6)
///
/// This is a simplified parser focused on extracting host/user for validation.
/// Full operand parsing happens in the SSH transport layer.
fn parse_remote_operand(operand: &str) -> Result<RemoteOperandParsed, ClientError> {
    let operand_str = operand.to_owned();

    let colon_pos = operand.rfind(':').ok_or_else(|| {
        invalid_argument_error(
            &format!("invalid remote operand: missing ':' in {operand}"),
            1,
        )
    })?;

    let host_part = &operand[..colon_pos];

    let (user, host_with_port) = if let Some(at_pos) = host_part.find('@') {
        let user = host_part[..at_pos].to_string();
        let host = &host_part[at_pos + 1..];
        (Some(user), host)
    } else {
        (None, host_part)
    };

    let host = host_with_port.to_owned();
    let port = None;

    Ok(RemoteOperandParsed {
        operand: operand_str,
        host,
        user,
        port,
    })
}

/// Validates that all remote operands are from the same host with consistent credentials.
///
/// # Errors
///
/// Returns error if:
/// - Different hosts are specified
/// - Different usernames are specified (or mixed explicit/implicit)
/// - Different ports are specified
fn validate_same_host(operands: &[RemoteOperandParsed]) -> Result<(), ClientError> {
    if operands.is_empty() {
        return Ok(());
    }

    let first = &operands[0];

    for operand in &operands[1..] {
        if operand.host != first.host {
            return Err(invalid_argument_error(
                &format!(
                    "all remote sources must be from the same host (found '{}' and '{}')",
                    first.host, operand.host
                ),
                1,
            ));
        }

        match (&operand.user, &first.user) {
            (Some(u1), Some(u2)) if u1 != u2 => {
                return Err(invalid_argument_error(
                    &format!("remote sources must use the same username (found '{u2}' and '{u1}')"),
                    1,
                ));
            }
            (Some(u), None) | (None, Some(u)) => {
                return Err(invalid_argument_error(
                    &format!("cannot mix explicit username ('{u}') with implicit username"),
                    1,
                ));
            }
            _ => {}
        }

        if operand.port != first.port {
            return Err(invalid_argument_error(
                "remote sources must use the same port",
                1,
            ));
        }
    }

    Ok(())
}

/// Determines the transfer type and role from source and destination operands.
///
/// Analyzes the operands to determine whether this is a push (local -> remote),
/// pull (remote -> local), or proxy (remote -> remote) transfer.
///
/// # Arguments
///
/// * `sources` - Source operand(s)
/// * `destination` - Destination operand
///
/// # Returns
///
/// A `TransferSpec` describing the transfer type with all relevant operands.
///
/// # Errors
///
/// Returns error if:
/// - Neither source nor destination is remote (should use local copy)
/// - Multiple sources with different remote/local mix
/// - Multiple remote sources from different hosts, users, or ports
pub fn determine_transfer_role(
    sources: &[OsString],
    destination: &OsString,
) -> Result<TransferSpec, ClientError> {
    let dest_is_remote = operand_is_remote(destination);

    let remote_sources: Vec<_> = sources.iter().filter(|s| operand_is_remote(s)).collect();

    let has_remote_source = !remote_sources.is_empty();
    let all_sources_remote = remote_sources.len() == sources.len();

    match (has_remote_source, dest_is_remote) {
        (true, true) => {
            if !all_sources_remote {
                return Err(invalid_argument_error(
                    "mixing remote and local sources is not supported",
                    1,
                ));
            }

            let parsed_sources: Result<Vec<_>, _> = sources
                .iter()
                .map(|s| parse_remote_operand(&s.to_string_lossy()))
                .collect();
            let parsed_sources = parsed_sources?;

            validate_same_host(&parsed_sources)?;

            let remote_sources = if sources.len() > 1 {
                RemoteOperands::Multiple(
                    sources
                        .iter()
                        .map(|s| s.to_string_lossy().to_string())
                        .collect(),
                )
            } else {
                RemoteOperands::Single(sources[0].to_string_lossy().to_string())
            };

            Ok(TransferSpec::Proxy {
                remote_sources,
                remote_dest: destination.to_string_lossy().to_string(),
            })
        }
        (false, false) => Err(invalid_argument_error("no remote operand found", 1)),
        (true, false) => {
            if !all_sources_remote {
                return Err(invalid_argument_error(
                    "mixing remote and local sources is not supported",
                    1,
                ));
            }

            let parsed_sources: Result<Vec<_>, _> = sources
                .iter()
                .map(|s| parse_remote_operand(&s.to_string_lossy()))
                .collect();
            let parsed_sources = parsed_sources?;

            validate_same_host(&parsed_sources)?;

            let local_dest = destination.to_string_lossy().to_string();

            let remote_sources = if sources.len() > 1 {
                RemoteOperands::Multiple(
                    sources
                        .iter()
                        .map(|s| s.to_string_lossy().to_string())
                        .collect(),
                )
            } else {
                RemoteOperands::Single(sources[0].to_string_lossy().to_string())
            };

            Ok(TransferSpec::Pull {
                remote_sources,
                local_dest,
            })
        }
        (false, true) => {
            let local_sources: Vec<String> = sources
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();

            Ok(TransferSpec::Push {
                local_sources,
                remote_dest: destination.to_string_lossy().to_string(),
            })
        }
    }
}
