#![deny(unsafe_code)]

//! Native server orchestration entry points invoked via the hidden `--server` mode.

mod config;
mod role;

use std::fmt;
use std::io::{Read, Write};

use crate::message::{Message, Role as MessageRole};
use crate::rsync_error;

pub use self::config::ServerConfig;
pub use self::role::ServerRole;

/// Represents an error raised while executing the native server flow.
#[derive(Debug)]
pub struct ServerError {
    exit_code: i32,
    message: Message,
}

impl ServerError {
    /// Creates a new error with the provided exit code and message.
    pub fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Helper for reporting unimplemented functionality.
    pub fn unavailable(text: impl fmt::Display) -> Self {
        let mut message = rsync_error!(1, "{}", text);
        message = message.with_role(MessageRole::Server);
        Self::new(1, message)
    }

    /// Exit code that should be surfaced to the caller.
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Fully formatted message describing the failure.
    pub const fn message(&self) -> &Message {
        &self.message
    }
}

/// Executes the server entry point over stdin/stdout streams.
pub fn run_server_stdio(
    _config: ServerConfig,
    _stdin: &mut dyn Read,
    _stdout: &mut dyn Write,
) -> Result<i32, ServerError> {
    Err(ServerError::unavailable(
        "native rsync server mode is not yet implemented",
    ))
}

/// Clamps exit codes to the supported range.
pub fn clamp_exit_code(code: i32) -> i32 {
    code.clamp(0, crate::client::MAX_EXIT_CODE)
}
