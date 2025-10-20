#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `client` module exposes the minimal orchestration entry points consumed
//! by the `rsync` CLI binary. The current implementation focuses on
//! deterministic error reporting while the data-transfer engine is still under
//! construction. The API models the configuration and error structures that
//! higher layers will reuse once full synchronisation support lands.
//!
//! # Design
//!
//! - [`ClientConfig`] encapsulates the caller-provided transfer arguments. A
//!   builder is offered so future options (e.g. logging verbosity) can be wired
//!   through without breaking call sites.
//! - [`run_client`] executes the client flow. It currently reports that the
//!   delta-transfer engine is unavailable, matching the observable behaviour of
//!   the workspace today.
//! - [`ClientError`] carries the exit code and fully formatted
//!   [`Message`](crate::message::Message) so binaries can surface diagnostics via
//!   the central rendering helpers.
//!
//! # Invariants
//!
//! - `ClientError::exit_code` always matches the exit code embedded in the
//!   [`Message`].
//! - `run_client` never panics and preserves the provided configuration even
//!   when reporting unsupported functionality.
//!
//! # Errors
//!
//! All failures are routed through [`ClientError`]. The structure implements
//! [`std::error::Error`], allowing integration with higher-level error handling
//! stacks without losing access to the formatted diagnostic.
//!
//! # Examples
//!
//! Running the client with any configuration currently yields a diagnostic that
//! reports the missing delta-transfer engine.
//!
//! ```
//! use rsync_core::client::{run_client, ClientConfig};
//!
//! let config = ClientConfig::builder().build();
//! let error = run_client(config).expect_err("client support is not implemented yet");
//!
//! assert_eq!(error.exit_code(), 1);
//! assert!(error.message().to_string().contains("delta-transfer engine"));
//! ```
//!
//! # See also
//!
//! - [`crate::message`] for the formatting utilities reused by the client
//!   orchestration.
//! - [`crate::version`] for the canonical version banner shared with the CLI.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;

use crate::message::{Message, Role};

/// Exit code returned when client functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;

/// Configuration describing the requested client operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfig {
    transfer_args: Vec<OsString>,
}

impl ClientConfig {
    /// Creates a new [`ClientConfigBuilder`].
    #[must_use]
    pub fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    /// Returns the raw transfer arguments provided by the caller.
    #[must_use]
    pub fn transfer_args(&self) -> &[OsString] {
        &self.transfer_args
    }

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }
}

/// Builder used to assemble a [`ClientConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
}

impl ClientConfigBuilder {
    /// Sets the transfer arguments that should be propagated to the engine.
    #[must_use]
    pub fn transfer_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.transfer_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Finalises the builder and constructs a [`ClientConfig`].
    #[must_use]
    pub fn build(self) -> ClientConfig {
        ClientConfig {
            transfer_args: self.transfer_args,
        }
    }
}

/// Error returned when the client orchestration fails.
#[derive(Clone, Debug)]
pub struct ClientError {
    exit_code: i32,
    message: Message,
}

impl ClientError {
    /// Creates a new [`ClientError`] from the supplied message.
    fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    #[must_use]
    pub fn message(&self) -> &Message {
        &self.message
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for ClientError {}

/// Runs the client orchestration using the provided configuration.
///
/// At present the delta-transfer engine has not been integrated, so this helper
/// reports a structured diagnostic mirroring the CLI's existing behaviour.
pub fn run_client(config: ClientConfig) -> Result<(), ClientError> {
    let message = if config.has_transfer_request() {
        Message::error(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            "client functionality is unavailable in this build: the delta-transfer engine has not been implemented yet",
        )
    } else {
        Message::error(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            "client functionality is unavailable in this build: the delta-transfer engine has not been implemented yet",
        )
    }
    .with_role(Role::Client)
    .with_source(crate::message_source!());

    Err(ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_transfer_arguments() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("source"), OsString::from("dest")])
            .build();

        assert_eq!(
            config.transfer_args(),
            &[OsString::from("source"), OsString::from("dest")]
        );
        assert!(config.has_transfer_request());
    }

    #[test]
    fn run_client_reports_feature_unavailable() {
        let config = ClientConfig::builder().build();
        let error = run_client(config).expect_err("client support is missing");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("delta-transfer engine has not been implemented")
        );
        assert!(error.message().to_string().contains("[client=3.4.1-rust]"));
    }
}
