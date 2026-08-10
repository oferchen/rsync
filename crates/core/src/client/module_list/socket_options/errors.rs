//! Error construction helpers for socket option failures.

use std::io;

use crate::client::error::invalid_argument_error;
use crate::client::{ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, SOCKET_IO_EXIT_CODE};
use crate::message::Role;
use crate::rsync_error;

/// Constructs an error for a failed `setsockopt` call.
pub(super) fn socket_option_error(name: &str, error: io::Error) -> ClientError {
    let rendered = format!("failed to set socket option {name}: {error}");
    let message = rsync_error!(SOCKET_IO_EXIT_CODE, rendered).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

/// Constructs an error for an unrecognized option name.
pub(super) fn unknown_option(name: &str) -> ClientError {
    invalid_argument_error(
        &format!("Unknown socket option {name}"),
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

/// Constructs an error when a fixed-value option is given a user value.
#[cfg(not(target_family = "windows"))]
pub(super) fn option_disallows_value(name: &str) -> ClientError {
    invalid_argument_error(
        &format!("syntax error -- {name} does not take a value"),
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}
