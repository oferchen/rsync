#![deny(unsafe_code)]

/// Identifies the role executed by the server process.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServerRole {
    /// Receiver role responsible for applying incoming data.
    Receiver,
    /// Generator role responsible for producing file lists and deltas.
    Generator,
}
