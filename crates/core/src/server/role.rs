//! Server roles negotiated through the `--server` entry point.

/// Enumerates the server-side pipeline roles.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServerRole {
    /// Receives data from the client and applies it to the local filesystem.
    Receiver,
    /// Generates file lists and delta streams to send back to the client.
    Generator,
}
