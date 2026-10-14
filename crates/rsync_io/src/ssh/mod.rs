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
//! use rsync_io::ssh::SshCommand;
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
//!
//! # Wire primitive
//!
//! On Unix the child's stdin and stdout are two `AF_UNIX` `SOCK_STREAM`
//! socketpairs, one per direction, exactly as upstream creates them
//! (upstream: pipe.c:57 `piped_child()`, util1.c:84 `fd_pair()`). If the
//! kernel refuses either pair the spawn falls back to anonymous pipes; the
//! chosen primitive and any fallback reason are traced at `--debug=connect`
//! and reported by [`SshConnection::wire_primitive`]. Windows has no
//! `socketpair(2)` and always uses pipes. The stderr channel is a separate
//! socketpair on Unix; see `aux_channel.rs`.
//!
//! The 2026-10-13 change to socketpairs supersedes the pipe disposition
//! recorded in `docs/audits/ssh-socketpair-vs-pipes.md` and
//! `docs/audits/ssh-single-socketpair-bidirectional.md`.
//!
//! # io_uring on the wire
//!
//! The socketpair ends are sockets, so on Linux [`SshConnection::split`]
//! can drive them through the `fast_io` io_uring `RECV`/`SEND` paths. It does
//! so only under an explicit `--io-uring`: on this `AF_UNIX` wire io_uring
//! measured no gain, so the default is plain `read`/`write`. When zero-copy
//! is also requested the writer asks for
//! `IORING_OP_SEND_ZC`; the kernel refuses it on `AF_UNIX` (`EOPNOTSUPP`),
//! which the writer learns once at setup and then sends with plain `SEND`.
//! The pipe fallback always uses `read`/`write`.

/// The single ssh_config tokeniser, shared by both config readers.
#[cfg(any(feature = "ssh-config-parse", feature = "embedded-ssh"))]
mod argv_split;
#[cfg(all(feature = "async-ssh", feature = "ssh-socketpair-stderr"))]
mod async_stderr_drain;
#[cfg(feature = "async-ssh")]
mod async_transport;
mod aux_channel;
mod builder;
/// The ssh_config file load order, shared by both config readers.
#[cfg(any(feature = "ssh-config-parse", feature = "embedded-ssh"))]
mod config_files;
#[cfg(feature = "ssh-config-parse")]
mod config_lookup;
/// The single ssh_config option table, shared by both config readers.
#[cfg(any(feature = "ssh-config-parse", feature = "embedded-ssh"))]
mod config_options;
mod connect;
mod connection;
/// Embedded SSH transport using the russh library.
pub mod embedded;
mod operand;
mod parse;
/// Cross-platform stderr aux-channel between the parent process and a spawned `ssh` child.
#[cfg(feature = "ssh-socketpair-stderr")]
pub mod socketpair_stderr;
mod stall;
mod wire;

#[cfg(all(feature = "async-ssh", feature = "ssh-socketpair-stderr"))]
pub use async_stderr_drain::{ASYNC_STDERR_BUFFER_CAP, AsyncStderrDrain, RingBuffer};
#[cfg(feature = "async-ssh")]
pub use async_transport::AsyncSshTransport;
pub use builder::{SshAddressFamily, SshCommand};
pub use connect::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_KEEPALIVE_MAX_FAILURES,
    KeepAliveConfig, SshConnectConfig,
};
pub use connection::{SshChildHandle, SshConnection, SshReader, SshWriter};
pub use operand::{RemoteOperand, RemoteOperandParseError, parse_ssh_operand};
pub use parse::{RemoteShellParseError, parse_remote_shell};
pub use wire::WirePrimitive;

#[cfg(test)]
mod tests;
