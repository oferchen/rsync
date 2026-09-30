//! Typed daemon connection lifecycle management.
//!
//! Provides `ConnectionState` and transition validation for the daemon
//! connection lifecycle: `Greeting -> ModuleSelect -> Authenticating ->
//! Transferring`, with teardown to `Closing` available at any point.
//!
//! The two edge kinds are separate operations so that neither hides the
//! other. `transition` enforces forward-only progression and returns a
//! `Result` a caller must handle; `close` is total - reachable from every
//! state - so it returns the state directly and leaves no error to discard.
//! `Closing` is terminal.
//!
//! # Usage
//!
//! ```
//! use daemon::connection::ConnectionState;
//!
//! let state = ConnectionState::Greeting;
//! let state = state.transition(ConnectionState::ModuleSelect).unwrap();
//! let state = state.transition(ConnectionState::Transferring).unwrap();
//! let state = state.close();
//! assert!(state.is_terminal());
//! ```

mod state;

pub use state::{ConnectionState, InvalidTransition};
