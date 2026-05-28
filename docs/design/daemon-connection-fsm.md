# Daemon Connection Lifecycle State Machine

## Overview

The daemon connection handler progresses through a fixed sequence of states
from greeting through transfer to close. Today these transitions are implicit
in the control flow of `handle_legacy_session` and `process_approved_module`.
This design introduces a typed `ConnectionState` enum that makes every valid
transition explicit and prevents illegal jumps at compile time where possible
and at runtime otherwise.

## States

```
Greeting --> ModuleSelect --> Authenticating --> Transferring --> Closing
    |             |                |                 |
    +------+------+--------+------+---------+-------+
           |               |                |
           v               v                v
        Closing          Closing          Closing
```

| State            | Description                                         |
|------------------|-----------------------------------------------------|
| `Greeting`       | Server sent `@RSYNCD: <ver>.<sub> <digests>\n`, waiting for client version response. |
| `ModuleSelect`   | Version exchanged; waiting for client to send module name or `#list`. |
| `Authenticating` | Module found and requires auth; challenge sent, waiting for response. |
| `Transferring`   | Auth passed (or not required); transfer engine running. |
| `Closing`        | Session ending - `@RSYNCD: EXIT` sent or connection dropped. Reachable from any state. |

## Valid transitions

| From             | To               | Trigger                                      |
|------------------|------------------|----------------------------------------------|
| `Greeting`       | `ModuleSelect`   | Client responds with `@RSYNCD: <version>`    |
| `Greeting`       | `Closing`        | Client sends `@RSYNCD: EXIT`, EOF, or I/O error |
| `ModuleSelect`   | `Authenticating` | Module found and requires authentication     |
| `ModuleSelect`   | `Transferring`   | Module found, no auth required, `@RSYNCD: OK` sent |
| `ModuleSelect`   | `Closing`        | Unknown module, access denied, `#list` served, or error |
| `Authenticating` | `Transferring`   | Auth succeeded, `@RSYNCD: OK` sent           |
| `Authenticating` | `Closing`        | Auth failed or denied                        |
| `Transferring`   | `Closing`        | Transfer completes (success or failure)       |

All other transitions are invalid and return `InvalidTransition`.

## API surface

```rust
/// Current state of a daemon connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConnectionState {
    Greeting,
    ModuleSelect,
    Authenticating,
    Transferring,
    Closing,
}

/// Error returned when a state transition is not permitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTransition {
    pub from: ConnectionState,
    pub to: ConnectionState,
}

impl ConnectionState {
    /// Attempts to transition to `next`, returning `Ok(next)` on success
    /// or `Err(InvalidTransition)` if the transition is illegal.
    pub fn transition(self, next: ConnectionState)
        -> Result<ConnectionState, InvalidTransition>;

    /// Returns all states reachable from the current state.
    pub fn valid_transitions(self) -> &'static [ConnectionState];

    /// Returns `true` if the connection is in a terminal state.
    pub fn is_terminal(self) -> bool;
}
```

## Constraints

- `Closing` is a terminal (absorbing) state - no transitions out.
- Every state can transition to `Closing`.
- Forward-only progression through the non-closing states: no returning to
  a previous state.
- The enum and transition logic live in `crates/daemon/src/connection/state.rs`
  and are re-exported from the daemon crate.
- This PR defines the type and validation API only. Wiring the enum into the
  existing session handler is a follow-up to keep the change small and
  reviewable.

## Error handling

`InvalidTransition` implements `Display` and `Error` manually (the daemon
crate avoids `thiserror` because `core` shadows the primitive `core` crate).
The display format is: `invalid daemon connection transition: {from:?} -> {to:?}`.

## Testing

Exhaustive matrix: every `(from, to)` pair is tested. Valid transitions
return `Ok`, invalid transitions return `Err(InvalidTransition)`. Additional
tests cover the full happy-path lifecycle and edge cases (double close,
re-entering previous states).
