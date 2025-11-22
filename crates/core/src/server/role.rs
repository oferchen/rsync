#![deny(unsafe_code)]

/// Server execution role selected by the remote client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerRole {
    /// Receiver role applies incoming updates to the local filesystem.
    Receiver,
    /// Generator role enumerates and streams file data to the client.
    Generator,
}
