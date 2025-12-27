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

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for Receiver variant
    #[test]
    fn receiver_debug_format() {
        let role = ServerRole::Receiver;
        let debug = format!("{role:?}");
        assert_eq!(debug, "Receiver");
    }

    #[test]
    fn receiver_clone() {
        let role = ServerRole::Receiver;
        let cloned = role;
        assert_eq!(role, cloned);
    }

    #[test]
    fn receiver_copy() {
        let role = ServerRole::Receiver;
        let copied = role;
        assert_eq!(role, copied);
    }

    // Tests for Generator variant
    #[test]
    fn generator_debug_format() {
        let role = ServerRole::Generator;
        let debug = format!("{role:?}");
        assert_eq!(debug, "Generator");
    }

    #[test]
    fn generator_clone() {
        let role = ServerRole::Generator;
        let cloned = role;
        assert_eq!(role, cloned);
    }

    #[test]
    fn generator_copy() {
        let role = ServerRole::Generator;
        let copied = role;
        assert_eq!(role, copied);
    }

    // Tests for equality
    #[test]
    fn receiver_equals_receiver() {
        assert_eq!(ServerRole::Receiver, ServerRole::Receiver);
    }

    #[test]
    fn generator_equals_generator() {
        assert_eq!(ServerRole::Generator, ServerRole::Generator);
    }

    #[test]
    fn receiver_not_equals_generator() {
        assert_ne!(ServerRole::Receiver, ServerRole::Generator);
    }

    #[test]
    fn generator_not_equals_receiver() {
        assert_ne!(ServerRole::Generator, ServerRole::Receiver);
    }
}
