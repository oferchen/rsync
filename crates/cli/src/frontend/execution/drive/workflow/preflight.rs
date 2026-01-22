#![deny(unsafe_code)]

use crate::frontend::{arguments::ProgramName, render_help};
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
#[cfg(any(not(feature = "acl"), not(feature = "xattr")))]
use super::super::messages::fail_with_custom_fallback;
use super::super::messages::fail_with_message;

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

pub(crate) fn maybe_print_help_or_version<Out>(
    show_help: bool,
    show_version: bool,
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
    } else if show_version {
        let report = VersionInfoReport::for_client_brand(program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            Some(1)
        } else {
            Some(0)
        }
    } else {
        None
    }
}

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

pub(crate) fn validate_feature_support<Err>(
    preserve_acls: bool,
    xattrs: Option<bool>,
    stderr: &mut MessageSink<Err>,
) -> Result<(), i32>
where
    Err: Write,
{
    // ACL support requires both Unix and the acl feature
    #[cfg(not(all(unix, feature = "acl")))]
    if preserve_acls {
        let message =
            rsync_error!(1, "POSIX ACLs are not supported on this client").with_role(Role::Client);
        let fallback = "POSIX ACLs are not supported on this client".to_string();
        return Err(fail_with_custom_fallback(message, fallback, stderr));
    }

    #[cfg(all(unix, feature = "acl"))]
    let _ = preserve_acls;

    // xattr support requires both Unix and the xattr feature
    #[cfg(not(all(unix, feature = "xattr")))]
    if xattrs.unwrap_or(false) {
        let message = rsync_error!(1, "extended attributes are not supported on this client")
            .with_role(Role::Client);
        let fallback = "extended attributes are not supported on this client".to_string();
        return Err(fail_with_custom_fallback(message, fallback, stderr));
    }

    #[cfg(all(unix, feature = "xattr"))]
    let _ = xattrs;

    // Suppress unused variable warning for stderr when all features are enabled on Unix
    let _ = stderr;

    Ok(())
}
