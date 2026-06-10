#![deny(unsafe_code)]

use crate::frontend::{arguments::ProgramName, render_help, render_lsm_status_text};
use core::{
    client::{BindAddress, TransferTimeout},
    message::Role,
    rsync_error,
    version::VersionInfoReport,
};
use logging_sink::MessageSink;
use protocol::ProtocolVersion;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::super::super::{
    parse_bind_address_argument, parse_protocol_version_arg, parse_timeout_argument,
};
#[cfg(any(
    not(all(any(unix, windows), feature = "acl")),
    not(all(any(unix, windows), feature = "xattr"))
))]
use super::super::messages::fail_with_custom_fallback;
use super::super::messages::fail_with_message;

/// Rejects `--password-file=-` combined with `--files-from=-` since both read stdin.
pub(crate) fn validate_stdin_sources_conflict<Err>(
    password_file: &Option<PathBuf>,
    files_from: &[OsString],
    stderr: &mut MessageSink<Err>,
) -> Result<(), i32>
where
    Err: Write,
{
    if password_file
        .as_deref()
        .is_some_and(|path| path == Path::new("-"))
        && files_from
            .iter()
            .any(|entry| entry.as_os_str() == OsStr::new("-"))
    {
        let message = rsync_error!(
            1,
            "--password-file=- cannot be combined with --files-from=- because both read from standard input"
        )
        .with_role(Role::Client);
        Err(fail_with_message(message, stderr))
    } else {
        Ok(())
    }
}

/// Parses the `--protocol` argument into a [`ProtocolVersion`].
pub(crate) fn resolve_desired_protocol<Err>(
    protocol: Option<&OsString>,
    stderr: &mut MessageSink<Err>,
) -> Result<Option<ProtocolVersion>, i32>
where
    Err: Write,
{
    match protocol {
        Some(value) => match parse_protocol_version_arg(value.as_os_str()) {
            Ok(version) => Ok(Some(version)),
            Err(message) => Err(fail_with_message(message, stderr)),
        },
        None => Ok(None),
    }
}

/// Parses a timeout argument into a [`TransferTimeout`].
pub(crate) fn resolve_timeout<Err>(
    value: Option<&OsString>,
    stderr: &mut MessageSink<Err>,
) -> Result<TransferTimeout, i32>
where
    Err: Write,
{
    match value {
        Some(raw) => match parse_timeout_argument(raw.as_os_str()) {
            Ok(setting) => Ok(setting),
            Err(message) => Err(fail_with_message(message, stderr)),
        },
        None => Ok(TransferTimeout::Default),
    }
}

/// Prints help, version, io_uring status, or LSM status text if requested,
/// returning the exit code.
///
/// `show_version` is a count: 1 = human-readable output (upstream `-V`),
/// 2+ = machine-readable JSON (upstream `-VV`).
// upstream: options.c:1940-1942 - version_opt_cnt selects JSON vs human-readable
pub(crate) fn maybe_print_help_or_version<Out>(
    show_help: bool,
    show_version: u8,
    show_io_uring_status: bool,
    show_lsm_status: bool,
    program_name: ProgramName,
    stdout: &mut Out,
) -> Option<i32>
where
    Out: Write,
{
    if show_help {
        let help = render_help(program_name);
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            Some(1)
        } else {
            Some(0)
        }
    } else if show_version >= 2 {
        let report = VersionInfoReport::for_client_brand(program_name.brand());
        let json = report.machine_readable();
        if stdout.write_all(json.as_bytes()).is_err() {
            Some(1)
        } else {
            Some(0)
        }
    } else if show_version == 1 {
        let report = VersionInfoReport::for_client_brand(program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            Some(1)
        } else {
            Some(0)
        }
    } else if show_io_uring_status {
        let matrix = fast_io::io_uring_capability_matrix();
        if writeln!(stdout, "{matrix}").is_err() {
            Some(1)
        } else {
            Some(0)
        }
    } else if show_lsm_status {
        let diagnostic = render_lsm_status_text(program_name);
        if stdout.write_all(diagnostic.as_bytes()).is_err() {
            Some(1)
        } else {
            Some(0)
        }
    } else {
        None
    }
}

/// Parses the `--address` argument into a [`BindAddress`].
pub(crate) fn resolve_bind_address<Err>(
    value: Option<&OsString>,
    stderr: &mut MessageSink<Err>,
) -> Result<Option<BindAddress>, i32>
where
    Err: Write,
{
    match value {
        Some(raw) => match parse_bind_address_argument(raw) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(message) => Err(fail_with_message(message, stderr)),
        },
        None => Ok(None),
    }
}

/// Validates that requested ACL/xattr features are available on this platform.
pub(crate) fn validate_feature_support<Err>(
    preserve_acls: bool,
    xattrs: Option<bool>,
    stderr: &mut MessageSink<Err>,
) -> Result<(), i32>
where
    Err: Write,
{
    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    if preserve_acls {
        let message =
            rsync_error!(1, "POSIX ACLs are not supported on this client").with_role(Role::Client);
        let fallback = "POSIX ACLs are not supported on this client".to_string();
        return Err(fail_with_custom_fallback(message, fallback, stderr));
    }

    #[cfg(all(any(unix, windows), feature = "acl"))]
    let _ = preserve_acls;

    #[cfg(not(all(any(unix, windows), feature = "xattr")))]
    if xattrs.unwrap_or(false) {
        let message = rsync_error!(1, "extended attributes are not supported on this client")
            .with_role(Role::Client);
        let fallback = "extended attributes are not supported on this client".to_string();
        return Err(fail_with_custom_fallback(message, fallback, stderr));
    }

    #[cfg(all(any(unix, windows), feature = "xattr"))]
    let _ = xattrs;

    #[cfg(all(any(unix, windows), feature = "acl", feature = "xattr"))]
    let _ = stderr;

    Ok(())
}
