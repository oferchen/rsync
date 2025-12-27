use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Role used in the trailer portion of an rsync message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// Sender role (`[sender=…]`).
    Sender,
    /// Receiver role (`[receiver=…]`).
    Receiver,
    /// Generator role (`[generator=…]`).
    Generator,
    /// Server role (`[server=…]`).
    Server,
    /// Client role (`[client=…]`).
    Client,
    /// Daemon role (`[daemon=…]`).
    Daemon,
}

impl Role {
    /// All trailer roles in the canonical ordering used by upstream diagnostics.
    ///
    /// The ordering mirrors how rsync labels emit trailer roles when rendering
    /// diagnostics. Higher layers that need to iterate over every possible role
    /// can depend on this constant rather than re-specifying the sequence.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::message::Role;
    ///
    /// let labels: Vec<&str> = Role::ALL
    ///     .into_iter()
    ///     .map(|role| role.as_str())
    ///     .collect();
    ///
    /// assert_eq!(labels, ["sender", "receiver", "generator", "server", "client", "daemon"]);
    /// ```
    #[doc(alias = "trailer roles")]
    pub const ALL: [Self; 6] = [
        Self::Sender,
        Self::Receiver,
        Self::Generator,
        Self::Server,
        Self::Client,
        Self::Daemon,
    ];

    /// Returns the lowercase trailer identifier used when formatting messages.
    ///
    /// The returned string matches the suffix rendered by upstream rsync. Keeping the
    /// mapping here allows higher layers to reuse the canonical spelling when
    /// constructing out-of-band logs or telemetry derived from
    /// [`Message`](crate::message::Message) instances.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::message::Role;
    ///
    /// assert_eq!(Role::Sender.as_str(), "sender");
    /// assert_eq!(Role::Receiver.as_str(), "receiver");
    /// assert_eq!(Role::Daemon.as_str(), "daemon");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
            Self::Generator => "generator",
            Self::Server => "server",
            Self::Client => "client",
            Self::Daemon => "daemon",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a [`Role`] from a string fails.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("unrecognised rsync message role")]
pub struct ParseRoleError {
    _private: (),
}

impl FromStr for Role {
    type Err = ParseRoleError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "sender" => Ok(Self::Sender),
            "receiver" => Ok(Self::Receiver),
            "generator" => Ok(Self::Generator),
            "server" => Ok(Self::Server),
            "client" => Ok(Self::Client),
            "daemon" => Ok(Self::Daemon),
            _ => Err(ParseRoleError { _private: () }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for Role::ALL constant
    #[test]
    fn all_contains_six_roles() {
        assert_eq!(Role::ALL.len(), 6);
    }

    #[test]
    fn all_starts_with_sender() {
        assert_eq!(Role::ALL[0], Role::Sender);
    }

    #[test]
    fn all_ends_with_daemon() {
        assert_eq!(Role::ALL[5], Role::Daemon);
    }

    // Tests for Role::as_str
    #[test]
    fn sender_as_str() {
        assert_eq!(Role::Sender.as_str(), "sender");
    }

    #[test]
    fn receiver_as_str() {
        assert_eq!(Role::Receiver.as_str(), "receiver");
    }

    #[test]
    fn generator_as_str() {
        assert_eq!(Role::Generator.as_str(), "generator");
    }

    #[test]
    fn server_as_str() {
        assert_eq!(Role::Server.as_str(), "server");
    }

    #[test]
    fn client_as_str() {
        assert_eq!(Role::Client.as_str(), "client");
    }

    #[test]
    fn daemon_as_str() {
        assert_eq!(Role::Daemon.as_str(), "daemon");
    }

    // Tests for Display trait
    #[test]
    fn display_matches_as_str() {
        for role in Role::ALL {
            assert_eq!(format!("{role}"), role.as_str());
        }
    }

    // Tests for FromStr trait
    #[test]
    fn parse_sender() {
        assert_eq!("sender".parse::<Role>().unwrap(), Role::Sender);
    }

    #[test]
    fn parse_receiver() {
        assert_eq!("receiver".parse::<Role>().unwrap(), Role::Receiver);
    }

    #[test]
    fn parse_generator() {
        assert_eq!("generator".parse::<Role>().unwrap(), Role::Generator);
    }

    #[test]
    fn parse_server() {
        assert_eq!("server".parse::<Role>().unwrap(), Role::Server);
    }

    #[test]
    fn parse_client() {
        assert_eq!("client".parse::<Role>().unwrap(), Role::Client);
    }

    #[test]
    fn parse_daemon() {
        assert_eq!("daemon".parse::<Role>().unwrap(), Role::Daemon);
    }

    #[test]
    fn parse_unknown_fails() {
        assert!("unknown".parse::<Role>().is_err());
    }

    #[test]
    fn parse_empty_fails() {
        assert!("".parse::<Role>().is_err());
    }

    #[test]
    fn parse_uppercase_fails() {
        assert!("SENDER".parse::<Role>().is_err());
    }

    // Tests for trait implementations
    #[test]
    fn role_is_clone() {
        let role = Role::Sender;
        let cloned = role;
        assert_eq!(role, cloned);
    }

    #[test]
    fn role_is_copy() {
        let role = Role::Sender;
        let copied = role;
        assert_eq!(role, copied);
    }

    #[test]
    fn role_debug_contains_variant_name() {
        let debug = format!("{:?}", Role::Sender);
        assert!(debug.contains("Sender"));
    }

    #[test]
    fn roles_are_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Role::Sender);
        set.insert(Role::Receiver);
        assert_eq!(set.len(), 2);
    }

    // Tests for ParseRoleError
    #[test]
    fn parse_role_error_display() {
        let err = "invalid".parse::<Role>().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unrecognised"));
    }

    #[test]
    fn parse_role_error_is_clone() {
        let err = "invalid".parse::<Role>().unwrap_err();
        let cloned = err.clone();
        assert_eq!(err, cloned);
    }
}
