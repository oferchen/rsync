#![deny(unsafe_code)]

/// Enumerates the primary roles an rsync server can take during a remote
/// invocation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServerRole {
    /// Accepts updates from the remote peer.
    Receiver,
    /// Generates file lists and deltas for the remote peer.
    Generator,
}
