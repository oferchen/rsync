#![deny(unsafe_code)]
//! Native server orchestration utilities.

use std::io::{self, Read, Write};

/// Server configuration derived from the compact `--server` flag string.
pub mod config;
/// Generator role implementation for sending file lists and signatures.
pub mod generator;
/// Receiver role implementation for applying deltas and creating files.
pub mod receiver;
/// Enumerations describing the role executed by the server process.
pub mod role;

pub use self::config::ServerConfig;
pub use self::generator::{GeneratorError, run_generator};
pub use self::receiver::{ReceiverError, run_receiver};
pub use self::role::ServerRole;

/// Executes the native server entry point over standard I/O.
///
/// The implementation performs the protocol handshake before dispatching to the
/// role-specific handler.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<i32> {
    match config.role {
        ServerRole::Receiver => {
            run_receiver(config, stdin, stdout)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
        }
        ServerRole::Generator => {
            run_generator(config, stdin, stdout)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests;
