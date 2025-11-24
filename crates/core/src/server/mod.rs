#![deny(unsafe_code)]
//! Native server orchestration utilities.

use std::io::{self, Read, Write};

use self::config::ServerConfig;
use self::role::ServerRole;

/// Server configuration derived from the compact `--server` flag string.
pub mod config;
/// Enumerations describing the role executed by the server process.
pub mod role;

/// Executes the native server entry point over standard I/O.
///
/// The implementation performs the protocol handshake before dispatching to the
/// role-specific handler. Native server roles are not yet available; the
/// function reports the unsupported state using a structured I/O error.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<i32> {
    let _ = (stdin, stdout, config.protocol);
    let message = match config.role {
        ServerRole::Receiver => "native receiver role is not yet implemented",
        ServerRole::Generator => "native generator role is not yet implemented",
    };

    Err(io::Error::new(io::ErrorKind::Unsupported, message))
}
//! Server orchestration entry points mirroring the client facade.

mod config;
mod role;

pub use self::config::ServerConfig;
pub use self::role::ServerRole;

#[cfg(test)]
mod tests;
