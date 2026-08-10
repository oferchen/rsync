//! Typed daemon connection lifecycle management.
//!
//! Provides `ConnectionState` and transition validation for the daemon
//! connection lifecycle: `Greeting -> ModuleSelect -> Authenticating ->
//! Transferring -> Closing`. Every state can also transition directly to
//! `Closing`.
//!
//! The state machine enforces forward-only progression through the
//! non-terminal states. `Closing` is absorbing - once entered, no further
//! transitions are permitted.
//!
//! # Usage
//!
//! ```
//! use daemon::connection::ConnectionState;
//!
//! let state = ConnectionState::Greeting;
//! let state = state.transition(ConnectionState::ModuleSelect).unwrap();
//! let state = state.transition(ConnectionState::Transferring).unwrap();
//! let state = state.transition(ConnectionState::Closing).unwrap();
//! assert!(state.is_terminal());
//! ```

mod state;

pub use state::{ConnectionState, InvalidTransition};
