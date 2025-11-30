#![deny(unsafe_code)]
//! Server roles negotiated through the `--server` entry point.

/// Identifies the role executed by the server process.
///
/// When a client invokes rsync with `--server`, it specifies whether the remote
/// side should act as a Receiver (accepting pushed data) or a Generator
/// (producing data for the client to pull).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServerRole {
    /// Receives data from the client and applies it to the local filesystem.
    ///
    /// This is the default role when `--sender` is not present on the server
    /// command line.
    Receiver,
    /// Generates file lists and delta streams to send back to the client.
    ///
    /// This role is activated when the server invocation includes `--sender`.
    Generator,
}
