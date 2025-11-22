#![deny(unsafe_code)]

//! Server-side orchestration for rsync protocol 32.
//!
//! This module mirrors the client-facing helpers exposed by [`crate::client`] but
//! is tailored for invocations of `rsync --server ...` where the local binary acts
//! as the remote endpoint. The current implementation establishes the negotiated
//! session framing and surfaces structured configuration while leaving the
//! generator/receiver role execution for subsequent phases.

mod config;
mod role;

pub use config::ServerConfig;
pub use role::ServerRole;

use std::io::{self, Read, Write};

use transport::{SessionHandshake, negotiate_session_from_stream, sniff_negotiation_stream};

/// Drives the rsync server over standard I/O streams.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<i32> {
    let mut combined = BorrowedStdio {
        reader: stdin,
        writer: stdout,
    };
    let negotiated = sniff_negotiation_stream(&mut combined)?;
    let mut session = negotiate_session_from_stream(negotiated, config.protocol())?;

    match config.role() {
        ServerRole::Receiver => run_receiver(config, &mut session),
        ServerRole::Generator => run_generator(config, &mut session),
    }
}

fn run_receiver(
    _config: ServerConfig,
    _session: &mut SessionHandshake<&mut BorrowedStdio<'_>>,
) -> io::Result<i32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "receiver role is not yet implemented",
    ))
}

fn run_generator(
    _config: ServerConfig,
    _session: &mut SessionHandshake<&mut BorrowedStdio<'_>>,
) -> io::Result<i32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "generator role is not yet implemented",
    ))
}

struct BorrowedStdio<'a> {
    reader: &'a mut dyn Read,
    writer: &'a mut dyn Write,
}

impl<'a> Read for BorrowedStdio<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl<'a> Write for BorrowedStdio<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
