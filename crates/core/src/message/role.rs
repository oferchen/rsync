use std::fmt;
use std::str::FromStr;

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
    /// use oc_rsync_core::message::Role;
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
    /// use oc_rsync_core::message::Role;
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRoleError {
    _private: (),
}

impl fmt::Display for ParseRoleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unrecognised rsync message role")
    }
}

impl std::error::Error for ParseRoleError {}

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
