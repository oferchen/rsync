# Type-state pattern for protocol-negotiation phases (#2134)

## Scope

Evaluate whether the daemon connection lifecycle, the protocol-negotiation
prologue, and the transfer phase machine should migrate from runtime state
enums and implicit linear function call ordering to compile-time type-state
types (where `Connection<Greeting>` and `Connection<Authenticating>` are
distinct types and invalid transitions become compile errors).

Companion design notes:

- `docs/design/zero-copy-chain-of-responsibility.md` (#2133)
- `docs/design/file-list-repository-pattern.md` (#2135)
- `docs/design/actor-pattern-generator-sender-receiver.md` (#2136)

Pre-existing related note: `docs/design/type-state-protocol-phases.md`
covers the same broad theme; this evaluation drills specifically into the
negotiation prologue (daemon handshake, `setup_protocol`, capability
exchange) which the earlier note treated only as candidate B.

## 1. Current state machines in the protocol and daemon paths

The grep below was the survey input
(`rg "enum [A-Za-z_]*(State|Phase|Step|Stage|Mode)" crates/protocol/
crates/daemon/ crates/transfer/`).

### 1.1 Runtime enums (no compile-time enforcement)

1. `crates/daemon/src/daemon/session_registry.rs:35` -
   `pub enum SessionState { Handshaking, Authenticating, Listing,
   Transferring, Completed, Failed }`. Stored in `SessionInfo` and
   mutated through `SessionRegistry::set_state`
   (`session_registry.rs:193`). Used exclusively for observability across
   a `DashMap<SessionId, SessionInfo>`. No transition validation: any
   `set_state` is accepted regardless of the previous value.
2. `crates/protocol/src/state/dynamic.rs:12` - `pub enum Phase {
   Negotiation, FileList, Transfer, Finalize }`, wrapped in
   `DynamicProtocolState`. Transitions go through
   `DynamicProtocolState::advance` (`dynamic.rs:116`) which validates
   prerequisites (`protocol_version`, `checksum_seed`, `file_count`)
   and returns `TransitionError` on missing data, but cannot prevent a
   caller from invoking the wrong setter at the wrong time.
3. `crates/protocol/src/xattr/entry.rs:13` - `pub enum XattrState`. Data
   classification of an xattr value, not a lifecycle phase; out of
   scope.
4. `crates/transfer/src/delta_pipeline.rs:336` - `enum ThresholdMode`.
   Strategy selector for delta-pipeline tuning, not a lifecycle phase;
   out of scope.

### 1.2 Compile-time type-state already in use

5. `crates/protocol/src/state/typestate.rs:14` - `pub struct
   ProtocolState<P: ProtocolPhase>` with markers `Negotiation`,
   `FileList`, `Transfer`, `Finalize` defined in
   `crates/protocol/src/state/phases.rs:13-79`. Consuming transitions
   `begin_file_list` (`typestate.rs:86`), `begin_transfer`
   (`typestate.rs:153`), `begin_finalize` (`typestate.rs:236`) hand
   the carried negotiated data into the next marker. Used as a
   tracking shell only - the actual wire I/O lives in
   `crates/transfer/src/setup`.
6. `crates/compress/src/strategy/type_state.rs:73` -
   `NegotiationPipeline<S>` with markers `Uninit`,
   `CapabilitiesExchanged`, `AlgorithmSelected` (`type_state.rs:44-54`).
   Three consuming transitions: `exchange_capabilities`,
   `select_algorithm`, `into_strategy`. The pipeline is the
   source-of-truth for which compressor the receiver instantiates and
   ships with `compile_fail` doctests that lock the guarantees in.

### 1.3 Implicit, function-call-ordered state machines

7. `crates/transfer/src/setup/mod.rs:67` - `setup_protocol` and
   `setup_protocol_with` (`setup/mod.rs:94`). The "state machine" is
   the body sequence: compat-flags exchange (when binary negotiation
   is active) -> capability negotiation -> checksum-seed exchange.
   There is no state object; the order is enforced by the linear
   function body. Mirrors upstream
   `compat.c:setup_protocol()`.
8. `crates/daemon/src/daemon/sections/session_runtime.rs:206` -
   `handle_legacy_session`. Writes the `@RSYNCD: <ver>` greeting at
   `session_runtime.rs:224`, reads the client version line and
   accumulates pre-module options in the loop at
   `session_runtime.rs:233-272`, then dispatches `#list` or module
   selection at `session_runtime.rs:276`. Lifecycle ordering is
   implicit: greet -> read version -> read options -> module-request
   -> hand off to transfer or list responder. No state object; the
   call sequence is the contract.
9. `crates/daemon/src/daemon/async_session/session.rs:129/175/212` -
   the async session writes the `SessionState` triple
   (`Handshaking` -> `Listing` -> `Completed`) into the registry as
   it advances. The lifecycle is the linear function body; the
   registry write is observability only.

### 1.4 Out-of-scope state-like enums

`crates/engine/src/local_copy/pipelined_state.rs:65`
(`PipelineState`), `crates/engine/src/local_copy/debug_del.rs:36`
(`DeletePhase`), the `recursive::DestinationState` and
`filter_program::SideState` enums - all are per-operation strategy or
debug discriminants rather than negotiation lifecycles, and none touch
the wire-ordering invariants this note evaluates.

### 1.5 Survey summary

| Kind                                       | Count | Where                                                  |
|--------------------------------------------|-------|---------------------------------------------------------|
| Runtime enum, no transition validation     | 1     | `SessionState`                                          |
| Runtime enum, transition-validated         | 1     | `DynamicProtocolState`                                  |
| Compile-time type-state already            | 2     | `ProtocolState<P>`, `NegotiationPipeline<S>`            |
| Implicit (linear function body)            | 3     | `setup_protocol`, `handle_legacy_session`, async session|

Six state machines are in scope for this evaluation: items 1, 2, 5, 6, 7,
8. Items 3, 4, and 1.4 are noted for completeness and excluded from the
recommendation.

## 2. Type-state sketch for the daemon handshake

Today, the daemon legacy handshake at
`crates/daemon/src/daemon/sections/session_runtime.rs:206` is a single
`fn handle_legacy_session(stream: TcpStream, ...) -> io::Result<()>`
body. Every step (greeting, version read, option accumulation, module
request, auth, transfer dispatch) is a sequence of statements in that
function, and the only thing forcing the order is that the next
statement reads what the previous one wrote.

A type-state shape for the same lifecycle would look like the following.
Marker types carry the data accumulated up to that point; transition
methods consume `self` and return either the next phase or a
`DaemonConnection<Closing>` that owns the failure reason.

```rust
// crates/daemon/src/daemon/connection_typestate.rs (sketch)

pub struct Greeting;
pub struct ModuleSelect {
    negotiated_protocol: u32,
    refused_options: Vec<String>,
    early_input: Option<Vec<u8>>,
}
pub struct Authenticating {
    negotiated_protocol: u32,
    module: ModuleName,
    challenge: ChallengeNonce,
}
pub struct Transferring {
    negotiated_protocol: u32,
    module: ModuleName,
    authenticated_user: Option<String>,
}
pub struct Closing {
    reason: CloseReason,
}

pub struct DaemonConnection<S> {
    wire: FramedWire,                   // read + write halves
    peer: SocketAddr,
    limiter: Option<BandwidthLimiter>,
    registry_handle: SessionId,         // for the observability mirror
    phase: S,
}

impl DaemonConnection<Greeting> {
    pub fn accept(stream: TcpStream, peer: SocketAddr, ...) -> Self { ... }

    pub fn send_greeting(
        mut self,
    ) -> io::Result<DaemonConnection<ModuleSelect>> {
        let greeting = legacy_daemon_greeting();
        self.wire.write_all(greeting.as_bytes())?;
        self.wire.flush()?;
        let (version, options, early_input) = read_client_prelude(&mut self.wire)?;
        Ok(self.into_phase(ModuleSelect {
            negotiated_protocol: version,
            refused_options: options,
            early_input,
        }))
    }
}

impl DaemonConnection<ModuleSelect> {
    pub fn read_module_request(
        mut self,
    ) -> io::Result<ModuleDispatch> {
        let line = read_trimmed_line(&mut self.wire)?.unwrap_or_default();
        match line.as_str() {
            "#list" => Ok(ModuleDispatch::List(self.into_phase(Listing { ... }))),
            "" => Ok(ModuleDispatch::Close(self.fail("empty module"))),
            name => Ok(ModuleDispatch::Module(
                self.into_phase(Authenticating::new(name.into(), ...)),
            )),
        }
    }
}

impl DaemonConnection<Authenticating> {
    pub fn challenge(
        mut self,
        response: AuthResponse,
    ) -> io::Result<Result<DaemonConnection<Transferring>, DaemonConnection<Closing>>> {
        if verify_response(&self.phase.challenge, response) {
            let user = response.user.clone();
            Ok(Ok(self.into_phase(Transferring {
                negotiated_protocol: self.phase.negotiated_protocol,
                module: self.phase.module.clone(),
                authenticated_user: Some(user),
            })))
        } else {
            Ok(Err(self.fail("auth")))
        }
    }
}

impl DaemonConnection<Transferring> {
    pub fn into_transfer(self) -> (Wire, TransferContext) { ... }
}

impl<S> DaemonConnection<S> {
    fn into_phase<N>(self, next: N) -> DaemonConnection<N> {
        // pushes `core::any::type_name::<N>()` into the registry mirror.
        DaemonConnection { wire: self.wire, peer: self.peer, ... , phase: next }
    }
    fn fail(self, reason: &str) -> DaemonConnection<Closing> { ... }
}
```

Calling `connection.read_module_request()` on a
`DaemonConnection<Greeting>` is a compile error because the method is
defined only on `DaemonConnection<ModuleSelect>`. The same holds for
`challenge` (only on `Authenticating`) and `into_transfer` (only on
`Transferring`). The existing `SessionState` registry stays unchanged
as the runtime mirror; the type-state owner pushes `core::any::type_name`
into it on every `into_phase` so the admin/stats view keeps working.

The same shape applies to `ProtocolState<P>` once it owns its own
read/write halves: the negotiation prologue becomes
`ProtocolState<Negotiation>::exchange_compat_flags ->
ProtocolState<Negotiation>::negotiate_capabilities ->
ProtocolState<Negotiation>::exchange_checksum_seed ->
ProtocolState<FileList>`, replacing the three statement-level calls
inside `setup_protocol_with`
(`crates/transfer/src/setup/mod.rs:103-179`).

## 3. Pros

- **Invalid transitions become compile errors.** The five common
  daemon-handshake interop regressions (skipped greeting, double-write
  of the module list, challenge before greeting, transfer started
  before authentication, close without final `@RSYNCD: EXIT`) are all
  ordering bugs in the body of `handle_legacy_session`. A type-state
  shape removes them at compile time instead of in
  `tcpdump`-and-bisect.
- **No runtime branch for "wrong state."** Today
  `DynamicProtocolState::advance` (`dynamic.rs:116-141`) checks
  `Option<u32>` for both `protocol_version` and `checksum_seed` on
  every advance. The type-state version carries `u32` directly in the
  next marker (see `phases.rs:35-44`) so the `Option` and the
  `TransitionError::Missing*` variants disappear from the hot path.
- **Self-documenting carried state.** `DaemonConnection<Transferring>`
  literally cannot exist without an `authenticated_user`. New
  contributors do not have to read the body of `handle_legacy_session`
  to learn which data is available at which step; the marker struct
  fields list it.
- **Drop is correct by construction.** The greeting socket is moved
  into every successor phase, so leaking the previous phase's wire is
  not expressible. Compare the manual `flush` and `drop` ordering in
  `session_runtime.rs:225-298`.

## 4. Cons

- **API surface explosion.** Each marker type needs its own `impl`
  block. A linear five-phase machine doubles when an optional branch
  is added (e.g. `Authenticating<Required>` vs
  `Authenticating<Skipped>` for unauthenticated modules). The
  `Closing` phase needs separate constructors from every error site
  (`fail_on_greeting`, `fail_on_auth`, `fail_on_module_select`) or it
  becomes an enum-of-reasons that re-introduces the runtime branch we
  were eliminating.
- **Generics in error returns.** Functions that today return
  `io::Result<()>` because the connection state is implicit must
  return `io::Result<DaemonConnection<Next>>` or
  `Result<DaemonConnection<Next>, DaemonConnection<Closing>>`. The
  outer caller cannot easily store "a connection in some phase" in a
  field without erasing the parameter, which is the exact escape-hatch
  reason `DynamicProtocolState` exists today
  (`state/mod.rs:31-33`).
- **Harder dynamic inspection.** The session registry mirror at
  `session_registry.rs:35` lives in a `DashMap<SessionId,
  SessionInfo>`. The `SessionInfo` struct cannot be generic over the
  type-state marker without erasing it back to an enum at the
  registry boundary, so the type-state shape still has to push a
  string or enum into the registry on every transition. That code is
  exactly what already exists; the type-state version does not remove
  it, only adds a layer of indirection.
- **Worse log lines and telemetry.** Today a single `phase: Phase` field
  on `DynamicProtocolState` (`dynamic.rs:62`) is `Display`-printable
  in one line. The type-state version requires either threading
  `core::any::type_name::<S>()` through every log site or carrying a
  runtime tag alongside the marker, which is the same enum the
  type-state was meant to replace.
- **Refactor blast radius.** The legacy daemon path
  (`handle_legacy_session`) and the async daemon path
  (`async_session/session.rs:118-220`) are two different bodies that
  would each need to be rewritten to consume and return typed
  connections. The async path additionally has to play nicely with
  `tokio::spawn`, which means the marker types must all be `Send` and
  the consuming transitions must be `async fn`. The `setup_protocol`
  call site
  (`crates/transfer/src/setup/mod.rs:67`) is called from both client
  and server paths; either the type-state shape supports both, or the
  shape lives behind a separate adapter that re-introduces the
  function-call ordering.
- **Cannot express the legacy / binary fork cleanly.**
  `session_runtime.rs:162-169` already documents that daemon mode is
  always legacy but the code retains a `SessionStyle::Binary` variant
  for the prepared binary path. A type-state shape would need either
  a shared `DaemonConnection<Greeting>` that branches on first read or
  two parallel marker hierarchies; both are more code than the
  current `match` on a parsed line.

## 5. Recommendation

**Adopt selectively, not across the board.**

Three concrete actions:

1. **Adopt for the daemon connection lifecycle
   (`session_runtime.rs:206` and `async_session/session.rs:118`).** This
   is the highest-risk implicit state machine in the codebase: ordering
   bugs here corrupt the wire silently and the recovery signal at the
   client is a generic "connection unexpectedly closed". The lifecycle
   is linear (`Greeting -> ModuleSelect -> {Listing | Authenticating}
   -> Transferring -> Closing`), single-threaded per connection, and
   owned by a single function body. Blast radius is bounded:
   `handle_legacy_session` plus the four-call dispatch under
   `module_access/`. The existing `SessionState` enum stays as the
   registry mirror; it is observability-only and the type-state
   transition methods push their `type_name` into it.

2. **Adopt for the negotiation prologue inside `setup_protocol`
   (`crates/transfer/src/setup/mod.rs:67`).** The three-step
   compat-flags / capability-negotiation / checksum-seed sequence is
   wire-ordering-critical, well-bounded, and already shadowed by the
   existing `ProtocolState<P>` type-state in
   `crates/protocol/src/state/typestate.rs:14`. Extend `ProtocolState`
   so the `Negotiation` marker owns the wire halves and exposes the
   three transition methods, then call it from `setup_protocol_with`
   instead of the current three free-function statements. The
   compression-negotiation pipeline at
   `crates/compress/src/strategy/type_state.rs:73` is the working
   reference for what the result should look like, including the
   `compile_fail` doctests that lock the guarantees in.

3. **Reject for the transfer phase (`Handshake -> FilterExchange ->
   FileListTransfer -> DeltaTransfer -> Finalization -> Complete`).**
   The transfer phase fans out across the sender, receiver, generator,
   and disk-commit threads (see
   `docs/design/actor-pattern-generator-sender-receiver.md` for the
   thread layout). A type-state object cannot cross these thread
   boundaries without erasing the marker back to an enum at every
   channel send. The realistic shape - type-state at the phase
   boundaries only, plain channels within - is what `core::session`
   already does in its linear body. Re-encoding the inter-thread
   layout in markers would force a refactor with no compile-time win.

4. **Reject for the runtime escape hatches.** `DynamicProtocolState`
   exists because `ProtocolState<P>` cannot live in a non-generic
   field (`state/mod.rs:31-33`); it is the right pattern for that
   problem. `SessionState` exists because the registry needs a single
   non-generic key into a `DashMap`; it is the right pattern for that
   problem. Both stay.

The summary table:

| State machine                              | File:line                                                       | Recommendation |
|--------------------------------------------|------------------------------------------------------------------|----------------|
| Daemon legacy session lifecycle            | `crates/daemon/src/daemon/sections/session_runtime.rs:206`       | Adopt          |
| Daemon async session lifecycle             | `crates/daemon/src/daemon/async_session/session.rs:118`          | Adopt          |
| `setup_protocol` negotiation prologue      | `crates/transfer/src/setup/mod.rs:67`                            | Adopt          |
| `ProtocolState<P>` (already type-stated)   | `crates/protocol/src/state/typestate.rs:14`                      | Extend         |
| `NegotiationPipeline<S>` (already)         | `crates/compress/src/strategy/type_state.rs:73`                  | Keep           |
| `DynamicProtocolState` runtime mirror      | `crates/protocol/src/state/dynamic.rs:12`                        | Keep as enum   |
| `SessionState` registry mirror             | `crates/daemon/src/daemon/session_registry.rs:35`                | Keep as enum   |
| Transfer phase across multiple workers     | `core::session` body                                             | Reject         |

The adoption order matches the risk per line of churn: daemon lifecycle
first (because the lifecycle is short, single-threaded, and the interop
bugs it would catch are the most expensive to debug), then the
negotiation prologue inside `setup_protocol` (because it is a single
function body and the marker types already exist). Each step ships
with `compile_fail` doctests modelled on
`crates/compress/src/strategy/type_state.rs` so the guarantees stay
locked.

## 6. Cross-references

- **#2133 - Zero-copy chain of responsibility**
  (`docs/design/zero-copy-chain-of-responsibility.md`). Orthogonal:
  CoR handles platform copy fallback inside the transfer phase; this
  note handles phase ordering around it. The two should compose - the
  transfer-phase type-state owner hands a wire-ready context to the
  CoR chain which then dispatches `copy_file_range`, `sendfile`, or
  the userspace fallback.
- **#2135 - File-list repository pattern**
  (`docs/design/file-list-repository-pattern.md`). Adjacent: once
  `ProtocolState<FileList>` owns the file list as carried state, the
  repository pattern is the natural query API for the carried list.
  Type-state owns the lifecycle, repository owns the access pattern.
- **#2136 - Actor pattern for generator / sender / receiver**
  (`docs/design/actor-pattern-generator-sender-receiver.md`). Directly
  motivates the reject in section 5 point 3: the transfer-phase
  workers are exactly the actors #2136 describes, and the type-state
  marker cannot cross the actor channel boundaries without being
  erased. The negotiation type-state hands off to the actor mesh at
  the `Transfer` boundary and the actors carry their own per-actor
  state.
- Pre-existing note: `docs/design/type-state-protocol-phases.md`.
  Covers the same broad theme; this note narrows specifically to the
  negotiation prologue and the daemon handshake and supplies the
  file:line citations and the sketch the earlier note deferred.
