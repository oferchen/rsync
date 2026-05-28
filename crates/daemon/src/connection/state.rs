//! Typed state enum and transition validation for the daemon connection
//! lifecycle.
//!
//! upstream: clientserver.c - the daemon connection progresses through
//! greeting, module selection, optional authentication, transfer, and close.
//! This module encodes those phases as a state machine with validated
//! transitions.

use std::error::Error;
use std::fmt;

/// Lifecycle state of a daemon connection.
///
/// Connections progress forward through the states in order. Every state
/// can transition to [`Closing`](Self::Closing), and `Closing` is terminal
/// - no further transitions are permitted.
///
/// # States
///
/// - **Greeting** - Server has sent the `@RSYNCD:` greeting and is waiting
///   for the client's version response.
/// - **ModuleSelect** - Version exchange complete; waiting for the client
///   to request a module name or `#list`.
/// - **Authenticating** - The requested module requires authentication;
///   a challenge has been sent and the server is waiting for the client's
///   response.
/// - **Transferring** - Authentication passed (or was not required) and
///   the transfer engine is running.
/// - **Closing** - The session is ending. Terminal state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConnectionState {
    /// Server sent `@RSYNCD: <ver>.<sub> <digests>\n`, awaiting client version.
    Greeting,
    /// Version exchanged; awaiting module name or `#list` from client.
    ModuleSelect,
    /// Module requires auth; challenge sent, awaiting client response.
    Authenticating,
    /// Auth passed or not required; transfer engine running.
    Transferring,
    /// Session ending. Terminal - no transitions out.
    Closing,
}

/// Error returned when a state transition is not permitted.
///
/// Contains the source and target states so callers can inspect exactly
/// which transition was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTransition {
    /// The state the connection was in when the transition was attempted.
    pub from: ConnectionState,
    /// The target state that was rejected.
    pub to: ConnectionState,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid daemon connection transition: {:?} -> {:?}",
            self.from, self.to
        )
    }
}

impl Error for InvalidTransition {}

impl ConnectionState {
    /// Returns `true` if this is a terminal state with no outgoing transitions.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closing)
    }

    /// Returns all states reachable from the current state.
    ///
    /// The returned slice is static and ordered by the natural progression
    /// of the protocol. `Closing` always appears last when present.
    #[must_use]
    pub const fn valid_transitions(self) -> &'static [ConnectionState] {
        match self {
            Self::Greeting => &[Self::ModuleSelect, Self::Closing],
            Self::ModuleSelect => &[Self::Authenticating, Self::Transferring, Self::Closing],
            Self::Authenticating => &[Self::Transferring, Self::Closing],
            Self::Transferring => &[Self::Closing],
            Self::Closing => &[],
        }
    }

    /// Attempts to transition to `next`.
    ///
    /// Returns `Ok(next)` when the transition is valid, or
    /// `Err(InvalidTransition)` when it is not. A transition is valid if
    /// `next` appears in [`valid_transitions`](Self::valid_transitions).
    ///
    /// # Examples
    ///
    /// ```
    /// use daemon::connection::ConnectionState;
    ///
    /// let state = ConnectionState::Greeting;
    /// assert!(state.transition(ConnectionState::ModuleSelect).is_ok());
    /// assert!(state.transition(ConnectionState::Transferring).is_err());
    /// ```
    pub const fn transition(self, next: ConnectionState) -> Result<ConnectionState, InvalidTransition> {
        // PartialEq::eq is not available in const context for custom enums,
        // so compare discriminants via as-cast.
        let next_disc = next as u8;
        let valid = self.valid_transitions();
        let mut i = 0;
        while i < valid.len() {
            if valid[i] as u8 == next_disc {
                return Ok(next);
            }
            i += 1;
        }
        Err(InvalidTransition {
            from: self,
            to: next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Valid transitions -------------------------------------------------

    #[test]
    fn greeting_to_module_select() {
        let result = ConnectionState::Greeting.transition(ConnectionState::ModuleSelect);
        assert_eq!(result, Ok(ConnectionState::ModuleSelect));
    }

    #[test]
    fn greeting_to_closing() {
        let result = ConnectionState::Greeting.transition(ConnectionState::Closing);
        assert_eq!(result, Ok(ConnectionState::Closing));
    }

    #[test]
    fn module_select_to_authenticating() {
        let result = ConnectionState::ModuleSelect.transition(ConnectionState::Authenticating);
        assert_eq!(result, Ok(ConnectionState::Authenticating));
    }

    #[test]
    fn module_select_to_transferring() {
        let result = ConnectionState::ModuleSelect.transition(ConnectionState::Transferring);
        assert_eq!(result, Ok(ConnectionState::Transferring));
    }

    #[test]
    fn module_select_to_closing() {
        let result = ConnectionState::ModuleSelect.transition(ConnectionState::Closing);
        assert_eq!(result, Ok(ConnectionState::Closing));
    }

    #[test]
    fn authenticating_to_transferring() {
        let result = ConnectionState::Authenticating.transition(ConnectionState::Transferring);
        assert_eq!(result, Ok(ConnectionState::Transferring));
    }

    #[test]
    fn authenticating_to_closing() {
        let result = ConnectionState::Authenticating.transition(ConnectionState::Closing);
        assert_eq!(result, Ok(ConnectionState::Closing));
    }

    #[test]
    fn transferring_to_closing() {
        let result = ConnectionState::Transferring.transition(ConnectionState::Closing);
        assert_eq!(result, Ok(ConnectionState::Closing));
    }

    // -- Invalid transitions -----------------------------------------------

    #[test]
    fn greeting_to_greeting() {
        let result = ConnectionState::Greeting.transition(ConnectionState::Greeting);
        assert_eq!(
            result,
            Err(InvalidTransition {
                from: ConnectionState::Greeting,
                to: ConnectionState::Greeting,
            })
        );
    }

    #[test]
    fn greeting_to_authenticating() {
        let result = ConnectionState::Greeting.transition(ConnectionState::Authenticating);
        assert!(result.is_err());
    }

    #[test]
    fn greeting_to_transferring() {
        let result = ConnectionState::Greeting.transition(ConnectionState::Transferring);
        assert!(result.is_err());
    }

    #[test]
    fn module_select_to_greeting() {
        let result = ConnectionState::ModuleSelect.transition(ConnectionState::Greeting);
        assert!(result.is_err());
    }

    #[test]
    fn module_select_to_module_select() {
        let result = ConnectionState::ModuleSelect.transition(ConnectionState::ModuleSelect);
        assert!(result.is_err());
    }

    #[test]
    fn authenticating_to_greeting() {
        let result = ConnectionState::Authenticating.transition(ConnectionState::Greeting);
        assert!(result.is_err());
    }

    #[test]
    fn authenticating_to_module_select() {
        let result = ConnectionState::Authenticating.transition(ConnectionState::ModuleSelect);
        assert!(result.is_err());
    }

    #[test]
    fn authenticating_to_authenticating() {
        let result = ConnectionState::Authenticating.transition(ConnectionState::Authenticating);
        assert!(result.is_err());
    }

    #[test]
    fn transferring_to_greeting() {
        let result = ConnectionState::Transferring.transition(ConnectionState::Greeting);
        assert!(result.is_err());
    }

    #[test]
    fn transferring_to_module_select() {
        let result = ConnectionState::Transferring.transition(ConnectionState::ModuleSelect);
        assert!(result.is_err());
    }

    #[test]
    fn transferring_to_authenticating() {
        let result = ConnectionState::Transferring.transition(ConnectionState::Authenticating);
        assert!(result.is_err());
    }

    #[test]
    fn transferring_to_transferring() {
        let result = ConnectionState::Transferring.transition(ConnectionState::Transferring);
        assert!(result.is_err());
    }

    #[test]
    fn closing_to_greeting() {
        let result = ConnectionState::Closing.transition(ConnectionState::Greeting);
        assert!(result.is_err());
    }

    #[test]
    fn closing_to_module_select() {
        let result = ConnectionState::Closing.transition(ConnectionState::ModuleSelect);
        assert!(result.is_err());
    }

    #[test]
    fn closing_to_authenticating() {
        let result = ConnectionState::Closing.transition(ConnectionState::Authenticating);
        assert!(result.is_err());
    }

    #[test]
    fn closing_to_transferring() {
        let result = ConnectionState::Closing.transition(ConnectionState::Transferring);
        assert!(result.is_err());
    }

    #[test]
    fn closing_to_closing() {
        let result = ConnectionState::Closing.transition(ConnectionState::Closing);
        assert!(result.is_err());
    }

    // -- Happy-path lifecycle -----------------------------------------------

    #[test]
    fn full_lifecycle_with_auth() {
        let state = ConnectionState::Greeting;
        let state = state.transition(ConnectionState::ModuleSelect).unwrap();
        let state = state.transition(ConnectionState::Authenticating).unwrap();
        let state = state.transition(ConnectionState::Transferring).unwrap();
        let state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn full_lifecycle_without_auth() {
        let state = ConnectionState::Greeting;
        let state = state.transition(ConnectionState::ModuleSelect).unwrap();
        let state = state.transition(ConnectionState::Transferring).unwrap();
        let state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    // -- Edge cases ---------------------------------------------------------

    #[test]
    fn early_close_from_greeting() {
        let state = ConnectionState::Greeting;
        let state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
        assert!(state.transition(ConnectionState::Greeting).is_err());
    }

    #[test]
    fn early_close_from_module_select() {
        let state = ConnectionState::ModuleSelect;
        let state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn double_close_is_invalid() {
        let state = ConnectionState::Closing;
        assert!(state.transition(ConnectionState::Closing).is_err());
    }

    #[test]
    fn cannot_reenter_previous_state_after_progress() {
        let state = ConnectionState::ModuleSelect;
        assert!(state.transition(ConnectionState::Greeting).is_err());

        let state = ConnectionState::Authenticating;
        assert!(state.transition(ConnectionState::ModuleSelect).is_err());
        assert!(state.transition(ConnectionState::Greeting).is_err());

        let state = ConnectionState::Transferring;
        assert!(state.transition(ConnectionState::Authenticating).is_err());
        assert!(state.transition(ConnectionState::ModuleSelect).is_err());
        assert!(state.transition(ConnectionState::Greeting).is_err());
    }

    // -- valid_transitions --------------------------------------------------

    #[test]
    fn valid_transitions_greeting() {
        let valid = ConnectionState::Greeting.valid_transitions();
        assert_eq!(valid, &[ConnectionState::ModuleSelect, ConnectionState::Closing]);
    }

    #[test]
    fn valid_transitions_module_select() {
        let valid = ConnectionState::ModuleSelect.valid_transitions();
        assert_eq!(
            valid,
            &[
                ConnectionState::Authenticating,
                ConnectionState::Transferring,
                ConnectionState::Closing,
            ]
        );
    }

    #[test]
    fn valid_transitions_authenticating() {
        let valid = ConnectionState::Authenticating.valid_transitions();
        assert_eq!(
            valid,
            &[ConnectionState::Transferring, ConnectionState::Closing]
        );
    }

    #[test]
    fn valid_transitions_transferring() {
        let valid = ConnectionState::Transferring.valid_transitions();
        assert_eq!(valid, &[ConnectionState::Closing]);
    }

    #[test]
    fn valid_transitions_closing() {
        let valid = ConnectionState::Closing.valid_transitions();
        assert!(valid.is_empty());
    }

    // -- is_terminal --------------------------------------------------------

    #[test]
    fn only_closing_is_terminal() {
        assert!(!ConnectionState::Greeting.is_terminal());
        assert!(!ConnectionState::ModuleSelect.is_terminal());
        assert!(!ConnectionState::Authenticating.is_terminal());
        assert!(!ConnectionState::Transferring.is_terminal());
        assert!(ConnectionState::Closing.is_terminal());
    }

    // -- InvalidTransition formatting ----------------------------------------

    #[test]
    fn invalid_transition_display() {
        let err = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Transferring,
        };
        let msg = format!("{err}");
        assert_eq!(
            msg,
            "invalid daemon connection transition: Greeting -> Transferring"
        );
    }

    #[test]
    fn invalid_transition_is_error() {
        let err = InvalidTransition {
            from: ConnectionState::Closing,
            to: ConnectionState::Greeting,
        };
        let _: &dyn Error = &err;
    }

    #[test]
    fn invalid_transition_debug() {
        let err = InvalidTransition {
            from: ConnectionState::Authenticating,
            to: ConnectionState::Greeting,
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("InvalidTransition"));
        assert!(debug.contains("Authenticating"));
        assert!(debug.contains("Greeting"));
    }

    #[test]
    fn invalid_transition_eq() {
        let a = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Transferring,
        };
        let b = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Transferring,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn invalid_transition_ne() {
        let a = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Transferring,
        };
        let b = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Authenticating,
        };
        assert_ne!(a, b);
    }

    // -- Clone / Copy / Hash -----------------------------------------------

    #[test]
    fn connection_state_clone_copy() {
        let state = ConnectionState::Greeting;
        let cloned = state;
        assert_eq!(state, cloned);
    }

    #[test]
    fn connection_state_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ConnectionState::Greeting);
        set.insert(ConnectionState::ModuleSelect);
        set.insert(ConnectionState::Authenticating);
        set.insert(ConnectionState::Transferring);
        set.insert(ConnectionState::Closing);
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn invalid_transition_clone_copy() {
        let err = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Closing,
        };
        let cloned = err;
        assert_eq!(err, cloned);
    }

    // -- Exhaustive transition matrix ----------------------------------------

    /// Verifies every possible (from, to) pair against the expected outcome.
    #[test]
    fn exhaustive_transition_matrix() {
        use ConnectionState::*;

        let all = [Greeting, ModuleSelect, Authenticating, Transferring, Closing];

        // Expected valid transitions encoded as (from, to) pairs.
        let valid_pairs: &[(ConnectionState, ConnectionState)] = &[
            (Greeting, ModuleSelect),
            (Greeting, Closing),
            (ModuleSelect, Authenticating),
            (ModuleSelect, Transferring),
            (ModuleSelect, Closing),
            (Authenticating, Transferring),
            (Authenticating, Closing),
            (Transferring, Closing),
        ];

        for &from in &all {
            for &to in &all {
                let result = from.transition(to);
                let expected_valid = valid_pairs.iter().any(|&(f, t)| f == from && t == to);
                assert_eq!(
                    result.is_ok(),
                    expected_valid,
                    "transition {from:?} -> {to:?}: expected valid={expected_valid}, got {:?}",
                    result
                );
            }
        }
    }
}
