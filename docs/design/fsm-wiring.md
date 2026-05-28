# FSM Wiring Design (FSW-1 through FSW-9)

Two typed finite state machines enforce protocol lifecycle ordering at runtime.
Both use forward-only transition validation - backward, self, and out-of-order
transitions return typed errors. This document covers the state machines, where
each transition fires in the orchestration code, error handling, and the
sole-authority pattern.

## 1. ConnectionState (daemon crate)

Tracks the daemon connection lifecycle from greeting through transfer to close.

### State diagram

```
                   +----------------+
                   |   Greeting     |
                   +-------+--------+
                           |
                           v
                   +----------------+
            +----->| ModuleSelect   |------+
            |      +-------+--------+      |
            |              |               |
            |        +-----+------+        |
            |        |            |        |
            |        v            v        |
            |  +-----------+ +----------+  |
            |  |Authenticat| |Transferr |  |
            |  |    ing    | |   ing    |  |
            |  +-----+-----+ +----+----+  |
            |        |            |        |
            |        +------+-----+        |
            |               |              |
            +-------+       v       +------+
                    |  +---------+  |
                    +->| Closing |<-+
                       +---------+
```

Every non-terminal state can transition directly to `Closing`. `Closing` is
absorbing - no outgoing transitions.

### Transition table

| From            | Valid targets                              |
|-----------------|--------------------------------------------|
| Greeting        | ModuleSelect, Closing                      |
| ModuleSelect    | Authenticating, Transferring, Closing      |
| Authenticating  | Transferring, Closing                      |
| Transferring    | Closing                                    |
| Closing         | (none - terminal)                          |

### Where transitions fire

| Transition                        | File                                                          | Line  | Trigger                                     |
|-----------------------------------|---------------------------------------------------------------|-------|----------------------------------------------|
| Greeting -> ModuleSelect         | `crates/daemon/src/daemon/sections/session_runtime.rs`        | 252   | Client version response received             |
| ModuleSelect -> Authenticating   | `crates/daemon/src/daemon/sections/module_access/request.rs`  | 180   | Module requires auth, challenge sent         |
| Authenticating -> Closing        | `crates/daemon/src/daemon/sections/module_access/request.rs`  | 198   | Auth failure, session ends                   |
| (any) -> Closing                 | `crates/daemon/src/daemon/sections/session_runtime.rs`        | 264   | Client-initiated `@RSYNCD: EXIT`             |
| ModuleSelect -> Closing          | `crates/daemon/src/daemon/sections/session_runtime.rs`        | 309   | `#list` response sent                        |
| (any) -> Closing                 | `crates/daemon/src/daemon/sections/session_runtime.rs`        | 322   | Empty request / handshake error              |
| (any) -> Transferring            | `crates/daemon/src/daemon/sections/module_access/transfer.rs` | 711   | Auth passed or not required, transfer begins |
| Transferring -> Closing          | `crates/daemon/src/daemon/sections/module_access/transfer.rs` | 746   | Transfer and post-xfer hooks complete        |

### API design

`ConnectionState` is a `Copy` enum. The `transition()` method is `const` and
returns `Result<ConnectionState, InvalidTransition>`:

```rust
let state = ConnectionState::Greeting;
let state = state.transition(ConnectionState::ModuleSelect)?;
```

Transition validation uses a static lookup table (`valid_transitions()`) that
returns a `&'static [ConnectionState]` slice for each state. The `transition()`
method linearly scans this slice to check if the target is permitted.

### Error type

`connection::InvalidTransition` stores `from` and `to` fields. Implements
`std::error::Error` and `Display`. Converted to `io::Error` with
`ErrorKind::InvalidData` via the `transition_error()` helper in
`daemon/sections/module_access/request.rs:12`.

---

## 2. TransferPipeline (transfer crate)

Tracks the transfer protocol phase lifecycle. Used by both Receiver and
Generator roles.

### State diagram

```
+------------+     +-----------------+     +-------------------+
| Handshake  |---->| FilterExchange  |---->| FileListTransfer  |
+------------+     +-----------------+     +-------------------+
                                                    |
                                                    v
  +----------+     +--------------+        +----------------+
  | Complete |<----| Finalization |<-------| DeltaTransfer  |
  +----------+     +--------------+        +----------------+
```

Strictly linear - each phase must be visited in order. No skipping, no
branching, no backward transitions.

### Phase sequence

| Ordinal | Phase             | Upstream reference                                      |
|---------|-------------------|---------------------------------------------------------|
| 0       | Handshake         | `compat.c:602-604` - version exchange                   |
| 1       | FilterExchange    | `exclude.c:recv_filter_list()`, `main.c:1258`           |
| 2       | FileListTransfer  | `flist.c:send_file_list()`, `flist.c:recv_file_list()`  |
| 3       | DeltaTransfer     | `receiver.c:recv_files()`, `sender.c:send_files()`      |
| 4       | Finalization      | `main.c:read_final_goodbye()`, `main.c:handle_stats()`  |
| 5       | Complete          | Terminal state                                           |

### Where transitions fire

#### Common entry point (both roles)

| Transition                        | File                                       | Line | Trigger                                     |
|-----------------------------------|--------------------------------------------|------|----------------------------------------------|
| (creation at Handshake)           | `crates/transfer/src/lib.rs`               | 319  | `TransferPipeline::new(config.role)`         |
| Handshake -> FilterExchange      | `crates/transfer/src/lib.rs`               | 431  | `setup_protocol()` completed                 |

#### Receiver role

| Transition                        | File                                                     | Line | Trigger                               |
|-----------------------------------|----------------------------------------------------------|------|----------------------------------------|
| FilterExchange -> FileListTransfer| `crates/transfer/src/receiver/transfer/setup.rs`         | 123  | Filter list reading complete           |
| FileListTransfer -> DeltaTransfer | `crates/transfer/src/receiver/transfer/setup.rs`         | 188  | File list received and sanitized       |
| DeltaTransfer -> Finalization     | `crates/transfer/src/receiver/transfer/phases.rs`        | 213  | Delta transfer loop complete           |
| Finalization -> Complete          | `crates/transfer/src/receiver/transfer/phases.rs`        | 252  | Goodbye handshake complete             |

#### Generator role

| Transition                        | File                                                     | Line | Trigger                               |
|-----------------------------------|----------------------------------------------------------|------|----------------------------------------|
| FilterExchange -> FileListTransfer| `crates/transfer/src/generator/transfer/orchestrator.rs` | 76   | Filter list exchange complete          |
| FileListTransfer -> DeltaTransfer | `crates/transfer/src/generator/transfer/orchestrator.rs` | 100  | File list sent                         |
| DeltaTransfer -> Finalization     | `crates/transfer/src/generator/transfer/orchestrator.rs` | 112  | Transfer loop complete                 |
| Finalization -> Complete          | `crates/transfer/src/generator/transfer/orchestrator.rs` | 218  | Goodbye handshake and diagnostics done |

### API design

`TransferPipeline` is a struct holding a `TransferPhase` enum and a
`ServerRole`. Three advancement methods:

- `advance()` - moves to the next phase (no target needed)
- `advance_to(target)` - single-step to an explicit target (must be exactly
  ordinal + 1)
- `advance_through(target)` - multi-step jump through intermediate phases

All return `Result<_, InvalidTransition>`.

### Error type

`transfer_state::InvalidTransition` stores `current` and `target` fields.
Derives `thiserror::Error` with a descriptive message including both phase
labels and a "forward-only" explanation. Converted to `io::Error` with
`ErrorKind::InvalidData` via the `fsm_error()` helper in
`crates/transfer/src/lib.rs:109`.

### Role-specific behavior

Both `ServerRole::Receiver` and `ServerRole::Generator` traverse the same six
phases in the same order. The role is carried for diagnostic context only - it
does not affect which transitions are valid.

The difference is where the transitions fire in the code:

- **Receiver**: setup reads filters, receives file list, runs the delta receive
  loop, then finalizes. Transitions fire in `setup.rs` (filter + flist) and
  `phases.rs` (delta + finalization).
- **Generator**: orchestrator reads filters, builds and sends file list, runs
  the transfer loop, then finalizes. All transitions fire in
  `orchestrator.rs`.

Both roles start from `FilterExchange` when handed to their context
constructors - the `Handshake -> FilterExchange` transition fires in the
shared `run_server_with_handshake` before dispatching to role-specific code.

---

## 3. Error handling

Both state machines convert invalid transitions to `io::Error` with
`ErrorKind::InvalidData`:

- **Daemon**: `transition_error()` in `module_access/request.rs:12`
- **Transfer**: `fsm_error()` in `transfer/src/lib.rs:109`

Both are thin wrappers that call `.to_string()` on the typed error and wrap it
in `io::Error::new(InvalidData, msg)`. The error propagates up through the
standard `?` operator, terminating the connection or transfer with a clear
message identifying the from/to states.

Invalid transitions indicate a logic error in the orchestration code, not a
protocol violation from the remote side. They should never occur in production.

---

## 4. Sole-authority pattern (FSW-7)

Each state machine is the single source of truth for its lifecycle phase.

### ConnectionState

The `conn_state` variable is threaded through the daemon session handler as a
mutable local. It is passed into `ModuleRequestContext` as the `conn_state`
field, which the module access code mutates through `ctx.conn_state`. There is
no other variable, flag, or enum tracking which protocol phase the daemon
connection is in.

The field's doc comment in `request.rs:38-39` explicitly states:

> Every phase transition goes through `ConnectionState::transition()`, which
> rejects invalid progressions. The field is the single source of truth for
> which protocol phase the connection is in.

### TransferPipeline

The `pipeline` field is stored on both `ReceiverContext` and
`GeneratorContext`. It is passed from `run_server_with_handshake` (where it is
created and advanced past Handshake) into the role-specific context constructor.
All subsequent phase transitions go through `self.pipeline.advance_to()`.

The field's doc comment in both `receiver/mod.rs:246-252` and
`generator/context.rs:93-98` states:

> Enforces the linear phase progression through the transfer lifecycle.
> Initialized at `FilterExchange` by `run_server_with_handshake` and advanced
> through `FileListTransfer`, `DeltaTransfer`, `Finalization`, and `Complete`
> as the [receiver/generator] progresses.

No other field tracks transfer phases. The FSM is the only mechanism that knows
and validates where the transfer is in its lifecycle.

---

## 5. Test coverage

### ConnectionState

- **Unit tests**: `crates/daemon/src/connection/state.rs` - 30+ tests covering
  every valid transition, every invalid transition (exhaustive 5x5 matrix),
  full lifecycle paths (with and without auth), early close, and error
  formatting.
- **Integration tests**: `crates/daemon/src/daemon/sections/session_runtime.rs`
  (lines 660-742) - FSM lifecycle tests embedded in the session runtime module,
  testing the same transitions in the context of the daemon session handler.

### TransferPipeline

- **Unit tests**: `crates/transfer/src/transfer_state.rs` (lines 272-714) -
  comprehensive tests for phase ordinals, next-phase chain, advance/advance_to/
  advance_through methods, every invalid backward/self/skip transition, error
  messages, and Debug formatting.
- **Wiring tests**: `crates/transfer/src/tests/transfer_pipeline_wiring.rs` -
  tests that simulate the exact phase sequence from `run_server_with_handshake`
  through receiver and generator lifecycles, plus invalid transition rejection
  and `fsm_error` conversion.

---

## 6. Source file index

| File                                                          | Purpose                                    |
|---------------------------------------------------------------|--------------------------------------------|
| `crates/daemon/src/connection/mod.rs`                         | Module declaration and re-exports          |
| `crates/daemon/src/connection/state.rs`                       | `ConnectionState` enum, transitions, tests |
| `crates/daemon/src/daemon/sections/session_runtime.rs`        | Session handler wiring (Greeting, exit)    |
| `crates/daemon/src/daemon/sections/module_access/request.rs`  | Module auth wiring (Auth, Closing)         |
| `crates/daemon/src/daemon/sections/module_access/transfer.rs` | Transfer wiring (Transferring, Closing)    |
| `crates/transfer/src/transfer_state.rs`                       | `TransferPipeline` and `TransferPhase`     |
| `crates/transfer/src/role.rs`                                 | `ServerRole` enum (Receiver, Generator)    |
| `crates/transfer/src/lib.rs`                                  | Pipeline creation, Handshake advance       |
| `crates/transfer/src/receiver/mod.rs`                         | `ReceiverContext.pipeline` field           |
| `crates/transfer/src/receiver/transfer/setup.rs`              | Receiver filter + flist transitions        |
| `crates/transfer/src/receiver/transfer/phases.rs`             | Receiver delta + finalization transitions  |
| `crates/transfer/src/generator/context.rs`                    | `GeneratorContext.pipeline` field          |
| `crates/transfer/src/generator/transfer/orchestrator.rs`      | Generator all four transitions             |
| `crates/transfer/src/tests/transfer_pipeline_wiring.rs`       | Transfer FSM wiring tests                  |
