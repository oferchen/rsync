# Type-State for Protocol Negotiation Phases (#2134)

## Summary

The type-state pattern encodes a state machine in the type system so that
illegal transitions become compile errors instead of runtime errors. We
already use type-state in two places (compression negotiation and the
top-level protocol lifecycle tracker) and an enum-based runtime tracker
in two more (`DynamicProtocolState`, `SessionState`). This note inventories
where we are today and proposes extending the pattern only to the two
highest-risk transition graphs - daemon connection lifecycle and transfer
setup - where ordering bugs cause silent wire-format corruption rather
than clean errors.

The recommendation is bounded: do not type-state every state machine.
Type-state has real costs (API churn, generic propagation, harder dynamic
dispatch) that only pay off where the underlying state machine is
linear, owned by a single thread, and where misordered calls produce
wire-format bugs rather than logic bugs that fail loudly.

## Inventory of state machines

### Already type-stated

1. `crates/protocol/src/state/typestate.rs` - `ProtocolState<P>`
   parameterized by phase markers `Negotiation`, `FileList`, `Transfer`,
   `Finalize`. Consuming `begin_file_list`, `begin_transfer`,
   `begin_finalize` transitions return new state instances. Validates
   prerequisites (protocol version, checksum seed, file count) at
   transition time. Used as a tracking shell - the actual negotiation
   wire I/O happens in `crates/transfer/src/setup` and the file-list
   transfer happens in `crates/transfer/src/generator` and consumes the
   tracker only for stats accounting.
2. `crates/compress/src/strategy/type_state.rs` -
   `NegotiationPipeline<S>` parameterized by `Uninit`,
   `CapabilitiesExchanged`, `AlgorithmSelected`. Three consuming
   transitions: `exchange_capabilities`, `select_algorithm`,
   `into_strategy`. The pipeline is the source-of-truth for which
   compressor the receiver instantiates, so misordered calls would
   produce a strategy mismatched to the negotiated algorithm. The
   compile-fail tests at the end of the file enumerate which mis-uses
   are caught at compile time.

### Runtime enum dispatch (no compile-time enforcement)

3. `crates/protocol/src/state/dynamic.rs` - `DynamicProtocolState` with
   a `Phase` enum (same four phases as the type-stated version).
   `advance()` validates prerequisites and mutates `self.phase`. The
   module doc says "useful when the phase needs to be tracked at
   runtime rather than enforced at compile time." This exists as an
   escape hatch when callers need to store the tracker behind a single
   non-generic type (e.g. in a struct field or a `dyn` boundary).
4. `crates/daemon/src/daemon/session_registry.rs` - `SessionState` enum
   (`Handshaking`, `Authenticating`, `Listing`, `Transferring`,
   `Completed`, `Failed`). Stored inside `SessionInfo` in a
   `DashMap<SessionId, SessionInfo>` for the daemon admin/stats view.
   No transition method - the daemon code that owns a session writes
   the new state directly via the registry. Misorderings here are
   invisible to the wire because the registry is observability-only.

### Implicit / function-call ordering

5. `crates/transfer/src/setup/mod.rs` - `setup_protocol` and
   `setup_protocol_with`. The "state machine" is the call sequence
   `exchange_compat_flags` -> capability negotiation -> checksum-seed
   exchange. There is no state object; the order is enforced by the
   linear function body. Mirrors upstream `compat.c:setup_protocol()`.
6. `crates/transfer/src/generator/*` and
   `crates/transfer/src/receiver/*` - the transfer phase itself
   (`Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer
   -> Finalization -> Complete` per the architecture description) is
   driven by the linear `core::session()` orchestration; there is no
   single state-object. Each step is its own function call.

### Verification of PR #1768

PR #1768 in this repo was the cleanup that removed a duplicate
`delay_updates` builder call and is unrelated to type-state. The
compression-negotiation type-state pipeline (`type_state.rs`) is
present and exercised by 30+ unit tests including five
`compile_fail` doctests that lock in the compile-time guarantees, so
the pattern is intact in code regardless of which PR introduced it.

## Why not type-state everything

The type-state pattern is most valuable when:

- The state machine is linear (a chain, not a graph with cycles or
  optional branches).
- The state object is owned by a single thread and not stored behind a
  trait object or in a heterogeneous collection.
- The cost of a misordered call is silent corruption of an external
  artefact (the wire, the disk) rather than a panic or an obvious
  logic bug.

It is **not** worth the cost when:

- The machine has cycles, optional steps, or per-call branches that
  would require an explosion of marker types (e.g. `Auth<Required>` vs
  `Auth<Skipped>` vs `Auth<Failed>` vs `Auth<Retrying>`).
- The state must round-trip through a `dyn` boundary, a `DashMap`, or
  an FFI surface. The whole point of `DynamicProtocolState` and
  `SessionRegistry` is to escape the generic parameter.
- The function-call order is already statically obvious from a 30-line
  linear body and adding markers would only obscure it.

The cost is real:

- Every transition consumes `self`, so callers that want to retain a
  reference to earlier-phase data must clone or stash it explicitly.
- Generic propagation: any function that wants to accept "a state in
  some phase" must be generic over `P: ProtocolPhase`, which infects
  call-site signatures and prevents storing the state in a non-generic
  field. The compression pipeline pays this cost; the rest of the
  codebase has so far avoided it.
- API churn: adding a new phase (or splitting one) becomes a
  breaking-API change rather than an enum-variant addition.

## Proposed candidates

Two state machines warrant the cost.

### Candidate A: Daemon connection lifecycle

**Current shape.** The daemon today has two views of the connection
state:

- `SessionState` enum in `session_registry.rs` (observability only).
- An implicit linear flow in the daemon accept loop:
  `Greeting` (`@RSYNCD: <ver>`) -> `ModuleSelect` (client sends module
  name or `#list`) -> `Authenticating` (challenge/response if the
  module requires auth) -> `Transferring` (handed to the transfer
  engine) -> `Closing`.

**Risk.** Misordering writes here corrupts the wire: writing the
challenge before the version is exchanged, writing module-list output
before module selection, or starting the transfer engine before
authentication completes. All of these would parse fine in Rust but
fail interop, often in ways that look like a generic "connection
unexpectedly closed" on the client.

**Proposal.** Introduce a `DaemonConnection<S>` in
`crates/daemon/src/daemon/` parameterized by markers `Greeting`,
`ModuleSelect`, `Authenticating`, `Transferring`, `Closing`. Each
marker carries the data accumulated up to that point (negotiated
protocol version, selected module name, authenticated user). The
state object owns the framed wire (read half + write half) and exposes
only the verbs valid for the current phase: `Greeting::send_greeting`,
`ModuleSelect::read_request`, `Authenticating::challenge`,
`Authenticating::verify`, etc.

Transition methods consume `self` and return either the next phase or
a `DaemonConnection<Closing>` carrying the failure reason. The
existing `SessionState` enum in the registry stays as the
observability mirror - the type-state object pushes its current phase
into the registry on each transition (this is the same pattern as
`DynamicProtocolState`: type-state for the owner, enum for the
observers).

**Cost.** Daemon connection handling is concentrated in
`daemon/sections/server_runtime/connection.rs` and a handful of
helpers under `daemon/sections/`. The blast radius is bounded because
the connection is single-threaded and never escapes to a `dyn` or
`DashMap`. Estimated 200-400 lines of API churn, plus mechanical
edits at the four to six call sites that drive the lifecycle.

**Benefit.** The five most common interop regressions in daemon mode
(skipped greeting on protocol-28 fallback, double-write of module
list, challenge written before greeting, transfer started before
authentication, close without final `@RSYNCD: EXIT`) all become
compile errors instead of `tcpdump`-and-bisect investigations.

### Candidate B: Transfer setup (handshake + filter + flist + delta + finalize)

**Current shape.** The transfer phase is driven by
`core::session()` which calls `setup_protocol`, then the filter
exchange, then `generator::run` / `receiver::run`, then a finalize
step. There is no state object; each step is a free function and
ordering is enforced by the linear `session` body.

**Risk.** The `setup_protocol -> filter_exchange -> flist_transfer ->
delta_transfer -> finalize` ordering must mirror upstream exactly.
Specific historical bugs:

- Filter rules sent before compat-flag exchange completed: client
  parses them as compat flags and aborts.
- File list started before checksum-seed exchange: receiver computes
  block checksums against the wrong seed.
- Delta phase entered before file list fully drained: the next
  varint is interpreted as a file index and the receiver writes to
  the wrong path.
- Finalize stats sent before delta phase drained: client sees a
  truncated transfer and reports success.

Each of these is a real upstream-fidelity hazard and each is harder
to catch in unit tests than a daemon-handshake bug because the state
is spread across the sender, receiver, and generator threads.

**Proposal.** A `TransferSession<S>` in `crates/transfer/src/` (or
`crates/core/src/session/`) parameterized by markers `Handshake`,
`FilterExchange`, `FlistTransfer`, `Delta`, `Finalize`. The state
object owns the negotiated protocol version, the compat flags, the
checksum seed, the filter chain, and the file list as each is
produced. Each phase exposes only the verbs valid for that phase.

This candidate is **harder than A** because the transfer phase fans
out into multiple threads (generator, sender/receiver, disk-commit).
The realistic shape is for the type-state object to gate the
*phase boundaries* only - the verbs that produce the next phase's
state - while individual workers within a phase use the concrete
sub-state owned by the marker. That keeps the generic parameter from
infecting every worker function.

**Cost.** Larger than A: ~500-800 lines, plus the harder design
question of how to carry the state across thread boundaries when each
phase has its own worker layout. The honest answer is that the
phase-boundary type-state is tractable but the within-phase
multi-threaded data flow is not, and trying to type-state the
within-phase work would require dependent types we do not have.

**Benefit.** Catches the four historical bugs above at compile time,
and gives a single place to assert "all data needed by phase N+1 was
produced by phase N." Removes the "ordering enforced by reading the
60-line `session` body" failure mode.

### Non-candidates (explicit)

- `SessionRegistry` / `SessionState`: stay as runtime enum. The whole
  point is observability across a `DashMap`.
- `DynamicProtocolState`: stay as runtime enum. It is already the
  escape-hatch that exists because the type-stated `ProtocolState<P>`
  cannot live in a non-generic field.
- The compression-negotiation pipeline: already type-stated, no
  change.
- Within-phase transfer worker layouts (rayon work queue, SPSC
  network-to-disk, ack batcher): linear function bodies; no payoff.
- CLI parsing and configuration (`CoreConfig` builder, etc.): builder
  pattern is the right pattern; type-state would force every optional
  flag to introduce a new marker.

## Cost / benefit summary

| Candidate                  | Risk today                                          | Cost (LoC + churn) | Benefit                                            | Recommend |
|----------------------------|------------------------------------------------------|---------------------|-----------------------------------------------------|-----------|
| A. Daemon connection       | Wire corruption on greeting/auth ordering           | 200-400 LoC, 4-6 call sites | Five common interop regressions become compile errors | Yes, do this first |
| B. Transfer setup          | Wire corruption on phase-boundary ordering          | 500-800 LoC, multi-thread coordination | Four historical bugs become compile errors at the phase boundary | Yes, after A lands |
| `SessionRegistry`          | Observability mismatch (no wire impact)             | n/a (already enum)  | None                                                | No        |
| `DynamicProtocolState`     | None - already an enum escape hatch                 | n/a                 | None                                                | No        |
| Within-phase workers       | Logic bugs that fail loudly                         | High - fans out to dependent types | Marginal                                            | No        |
| CLI / config builders      | None - builder pattern fits                         | High - flag-per-marker explosion | Negative (loses ergonomics)                       | No        |

## Migration order

1. Daemon connection (Candidate A). Bounded blast radius, single
   thread, highest interop risk per LoC.
2. Transfer setup phase boundaries (Candidate B), once A is settled
   and the within-phase boundary question (state hand-off across
   threads) has been answered by A's design.
3. Re-evaluate. If A and B do not catch a meaningful number of bugs
   in CI over six months of churn, do not extend further. The
   compression pipeline's compile-fail tests are the existing
   yardstick: every type-state introduction should ship with
   equivalent compile-fail doctests so the guarantees stay locked.

## References

- `crates/protocol/src/state/typestate.rs` - existing protocol-level
  type-state.
- `crates/compress/src/strategy/type_state.rs` - existing
  compression-negotiation type-state with compile-fail doctests.
- `crates/protocol/src/state/dynamic.rs` - the runtime-enum
  escape-hatch counterpart.
- `crates/daemon/src/daemon/session_registry.rs` - observability-only
  `SessionState` enum.
- `crates/transfer/src/setup/mod.rs` - linear function-call ordering
  for the protocol setup phase, the natural starting point for
  Candidate B.
- Upstream `compat.c:setup_protocol()` (target/interop/upstream-src/
  rsync-3.4.1/compat.c) - the wire-ordering invariant Candidate B
  must preserve.
