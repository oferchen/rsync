#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The [`SshCommand`] builder provides a thin wrapper around spawning the
//! system `ssh` (or compatible) binary.  The struct follows a builder pattern:
//! callers configure authentication parameters, additional command-line
//! options, and the remote command to execute before requesting a
//! [`SshConnection`].  The resulting connection implements [`std::io::Read`] and
//! [`std::io::Write`], allowing higher layers to treat the remote shell exactly like any
//! other byte stream when negotiating rsync sessions.
//!
//! # Design
//!
//! - [`SshCommand`] defaults to the `ssh` binary, enabling batch mode by
//!   default so password prompts never block non-interactive invocations.
//! - Builder-style setters expose user/host pairs, port selection, additional
//!   `ssh` options, and remote command arguments without forcing allocations in
//!   hot paths.
//! - [`SshConnection`] owns the spawned child process and forwards read/write
//!   operations to the child's stdout/stdin.  Dropping the connection flushes
//!   and closes the input pipe before reaping the child to avoid process leaks.
//!
//! # Examples
//!
//! Spawn the local SSH client and stream data to a remote rsync daemon.  The
//! example is marked `no_run` because it requires a reachable host.
//!
//! ```no_run
//! use transport::ssh::SshCommand;
//! use std::io::{Read, Write};
//!
//! let mut command = SshCommand::new("files.example.com");
//! command.set_user("backup");
//! command.push_remote_arg("rsync");
//! command.push_remote_arg("--server");
//! command.push_remote_arg("--sender");
//! command.push_remote_arg(".");
//!
//! let mut connection = command.spawn().expect("spawn ssh");
//! connection
//!     .write_all(b"@RSYNCD: 32.0\n")
//!     .expect("send greeting");
//! connection.flush().expect("flush transport");
//!
//! let mut response = Vec::new();
//! connection
//!     .read_to_end(&mut response)
//!     .expect("read daemon response");
//! ```
//!
//! # See also
//!
//! - [`crate::negotiate_session`] for the negotiation façade that consumes
//!   [`SshConnection`] streams.

mod builder;
mod connection;
mod parse;

pub use builder::SshCommand;
pub use connection::SshConnection;
pub use parse::{RemoteShellParseError, parse_remote_shell};

#[cfg(test)]
mod tests;
