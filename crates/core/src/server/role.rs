#![deny(unsafe_code)]

/// Identifies the server-side role used during a remote rsync invocation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServerRole {
    /// Acts as the receiver, applying updates sent by the client.
    Receiver,
    /// Acts as the generator/sender, producing file data for the client.
    Generator,
}
