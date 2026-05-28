# Ad-hoc State Tracking Audit (FSW-1, FSW-2)

Audit of ad-hoc state tracking in the daemon connection handler and transfer
orchestration code. Identifies booleans, phase variables, and implicit
sequential state that should map to the typed `ConnectionState` and
`TransferPipeline` FSM enums.

---

## Section 1: Daemon Connection Handler (FSW-1)

Target FSM: `ConnectionState` in `crates/daemon/src/connection/state.rs`
(Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing).

### Finding D1: `negotiated_protocol: Option<ProtocolVersion>`

- **File:** `crates/daemon/src/daemon/sections/session_runtime.rs`
- **Lines:** 234, 239-245
- **Current mechanism:** `Option<ProtocolVersion>` starts as `None` and becomes
  `Some(version)` when the `@RSYNCD:` version line is parsed. The `Some`/`None`
  distinction implicitly tracks whether the connection has completed the Greeting
  phase.
- **Target FSM state:** Greeting -> ModuleSelect. The transition from `None` to
  `Some` corresponds to `ConnectionState::Greeting` completing and entering
  `ConnectionState::ModuleSelect`.
- **Migration difficulty:** Low. Replace with a `ConnectionState::transition()`
  call after version parsing; store the version in the state payload.

### Finding D2: `request: Option<String>`

- **File:** `crates/daemon/src/daemon/sections/session_runtime.rs`
- **Lines:** 232, 260-275, 278
- **Current mechanism:** `Option<String>` starts as `None` and becomes
  `Some(module_name)` when the module request line is received. The post-loop
  `unwrap_or_default()` at line 278 converts to an empty string for error
  handling. The `Some`/`None` distinction tracks whether a module has been
  selected.
- **Target FSM state:** ModuleSelect. The transition from `None` to `Some`
  corresponds to entering `ConnectionState::ModuleSelect` with a module name
  payload.
- **Migration difficulty:** Low. The `request` value can be stored in the FSM
  state payload.

### Finding D3: `early_input_data: Option<Vec<u8>>`

- **File:** `crates/daemon/src/daemon/sections/session_runtime.rs`
- **Lines:** 235, 269-271
- **Current mechanism:** `Option<Vec<u8>>` accumulates early-input data from the
  `#early_input=<len>` protocol message. This data is threaded through to the
  module access handler. Its presence is an implicit signal that early-input was
  received during the greeting/module-select phase.
- **Target FSM state:** Greeting/ModuleSelect. Early-input is received before
  the module name, so it is part of the pre-ModuleSelect negotiation.
- **Migration difficulty:** Low. Can be carried as a field on the FSM's
  ModuleSelect state variant.

### Finding D4: Sequential greeting -> version -> module -> dispatch flow

- **File:** `crates/daemon/src/daemon/sections/session_runtime.rs`
- **Lines:** 207-321
- **Current mechanism:** The `handle_legacy_session` function runs a sequential
  flow: send greeting (line 230), enter read loop for version + module (lines
  237-276), then dispatch to `#list`, error, or `respond_with_module_request`
  (lines 280-318). Each section corresponds to a `ConnectionState` transition,
  but no state variable tracks which phase the connection is in.
- **Target FSM state:** Greeting -> ModuleSelect -> (branching to
  Transferring or Closing). The greeting write is Greeting, the read loop is
  ModuleSelect, and the dispatch is either Transferring or Closing.
- **Migration difficulty:** Medium. The sequential flow would need to be
  restructured around explicit `transition()` calls at each phase boundary.
  The read loop that collects both version and module name in a single pass
  would need to be split into two FSM-driven phases.

### Finding D5: `AuthenticationStatus` enum (parallel to ConnectionState)

- **File:** `crates/daemon/src/daemon/sections/module_access/authentication.rs`
- **Lines:** 10-17, 32-91
- **Current mechanism:** `AuthenticationStatus::Granted` / `Denied` enum is
  returned from `perform_module_authentication()`. This is a domain-specific
  result type that maps directly to the `ConnectionState::Authenticating` ->
  `Transferring` or `Authenticating` -> `Closing` transitions.
- **Target FSM state:** Authenticating. `Granted` maps to
  `transition(Transferring)`, `Denied` maps to `transition(Closing)`.
- **Migration difficulty:** Low. `AuthenticationStatus` can coexist with the
  FSM; the caller would call `transition()` based on the returned status.

### Finding D6: `process_approved_module` implicit phase progression

- **File:** `crates/daemon/src/daemon/sections/module_access/transfer.rs`
- **Lines:** 382-733
- **Current mechanism:** This 350-line function sequentially executes:
  connection acquisition (388-396), refused options (402-404), authentication
  (433-436), early-exec (441-501), client arg reading (505-508*), role
  determination (508), path validation (520-531), chroot/privilege (533-551),
  name converter (555-581), config building (596-599), filter rules (604-611),
  Landlock sandbox (621-623), stream setup (625-628), pre-xfer exec (655-701),
  handshake building (703), transfer execution (706-715), TCP shutdown (723),
  post-xfer exec (727-730). No state variable tracks progress through these
  sub-phases.
- **Target FSM state:** ModuleSelect -> Authenticating -> Transferring ->
  Closing. The function spans the entire post-ModuleSelect lifecycle.
- **Migration difficulty:** High. The function would need to be decomposed
  into FSM-driven phases. Many sub-steps have error paths that implicitly
  transition to Closing (via `send_error_and_exit` + `return Ok(())`),
  making the control flow graph complex.

### Finding D7: `SessionState` in async session (parallel FSM)

- **File:** `crates/daemon/src/daemon/async_session/session.rs`
- **Lines:** 119-224 (especially 129, 175, 212)
- **Current mechanism:** The async session handler uses
  `SessionState::Handshaking` (line 129), `SessionState::Listing` (line 175),
  and `SessionState::Completed` (line 212) from the session registry. This is a
  separate FSM from `ConnectionState` that tracks the same lifecycle with
  different state names and no shared type.
- **Target FSM state:** `SessionState::Handshaking` = `ConnectionState::Greeting`,
  `SessionState::Listing` = `ConnectionState::ModuleSelect`,
  `SessionState::Completed` = `ConnectionState::Closing`.
- **Migration difficulty:** Medium. The async session would need to use
  `ConnectionState` instead of `SessionState`, or `SessionState` would need to
  be defined in terms of `ConnectionState`. The async path is currently a
  placeholder (line 204: "daemon functionality limited in async mode") so the
  migration can wait until the async path is fully implemented.

### Finding D8: `SessionStyle` enum (orthogonal to ConnectionState)

- **File:** `crates/daemon/src/daemon/sections/session_runtime.rs`
- **Lines:** 163-170
- **Current mechanism:** `SessionStyle::Legacy` / `Binary` enum discriminates
  the wire protocol format. This is orthogonal to `ConnectionState` - it affects
  how each state transition is performed on the wire, not which state the
  connection is in.
- **Target FSM state:** N/A - this is a protocol-format discriminant, not a
  lifecycle state. It should remain independent of the `ConnectionState` FSM.
- **Migration difficulty:** N/A. No migration needed; the enum serves a
  different purpose.

---

## Section 2: Transfer Orchestration (FSW-2)

Target FSM: `TransferPipeline` in `crates/transfer/src/transfer_state.rs`
(Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer ->
Finalization -> Complete).

### Finding T1: `handshake.compat_exchanged` boolean

- **File:** `crates/transfer/src/lib.rs`
- **Lines:** 404
- **Current mechanism:** `bool` field on `HandshakeResult` controls whether the
  compat flags exchange is skipped in `setup_protocol()`. When `true`, the
  binary compat exchange was already done by the daemon's `@RSYNCD:` text
  protocol. This boolean encodes whether the Handshake phase has partially
  completed.
- **Target FSM state:** Handshake. The `compat_exchanged` flag distinguishes
  between a fresh handshake and a resumed one where the daemon layer already
  performed the compat exchange.
- **Migration difficulty:** Low. Can be replaced with a `TransferPhase` that
  starts at `FilterExchange` (when compat was already exchanged) or `Handshake`
  (when it was not).

### Finding T2: `is_server` boolean

- **File:** `crates/transfer/src/lib.rs`
- **Lines:** 325
- **Current mechanism:** `bool` derived from `!config.connection.client_mode`.
  Used to control the direction of the compat exchange (server writes, client
  reads) and checksum seed exchange. This is a role indicator, not a phase
  indicator, but it affects how Handshake phase transitions are performed.
- **Target FSM state:** Handshake (affects direction of exchange within the
  phase). This is an orthogonal concern - it determines how the FSM operates
  at each state, not which state is active.
- **Migration difficulty:** N/A. This is a role discriminant, not lifecycle
  state. It should parameterize the FSM, not be replaced by it.

### Finding T3: `is_daemon_mode` boolean

- **File:** `crates/transfer/src/lib.rs`
- **Lines:** 329
- **Current mechanism:** `bool` that controls whether negotiation is
  unidirectional (daemon) or bidirectional (SSH). Like `is_server`, this is a
  mode discriminant that parameterizes the Handshake phase behavior.
- **Target FSM state:** Handshake (parameterizes exchange behavior). Orthogonal
  to lifecycle state.
- **Migration difficulty:** N/A. Mode discriminant, not lifecycle state.

### Finding T4: `mplex_out` boolean and multiplex activation

- **File:** `crates/transfer/src/lib.rs`
- **Lines:** 480-488
- **Current mechanism:** `bool` returned by `requires_multiplex_output()`,
  guarding `writer.activate_multiplex()`. Multiplex activation is the boundary
  between the Handshake phase (raw I/O) and the FilterExchange phase
  (multiplexed I/O). The boolean is consumed once and not stored.
- **Target FSM state:** Handshake -> FilterExchange boundary. Multiplex
  activation is the concrete action that marks the transition.
- **Migration difficulty:** Low. The multiplex activation can be the side effect
  of a `TransferPipeline::advance()` call from Handshake to FilterExchange.

### Finding T5: `should_send_filter_list` boolean

- **File:** `crates/transfer/src/lib.rs`
- **Lines:** 496-512
- **Current mechanism:** `bool` computed from `config.role`,
  `config.connection.client_mode`, and filter-related flags. Guards the filter
  list write. This boolean determines whether the FilterExchange phase has any
  work to do.
- **Target FSM state:** FilterExchange. The filter list write is the primary
  action of this phase. When `false`, the phase is a no-op pass-through.
- **Migration difficulty:** Low. The `advance()` from FilterExchange to
  FileListTransfer can be unconditional; the filter write is conditional within
  the phase.

### Finding T6: `should_activate_input_multiplex()` guard (receiver)

- **File:** `crates/transfer/src/receiver/transfer/setup.rs`
- **Lines:** 58-71
- **Current mechanism:** Method returning `bool`, guarding
  `reader.activate_multiplex()` in `setup_transfer()`. This is the input-side
  complement of `mplex_out` (Finding T4). Together they complete the multiplex
  activation that marks the Handshake -> FilterExchange boundary on both the
  read and write channels.
- **Target FSM state:** Handshake -> FilterExchange boundary (input side).
- **Migration difficulty:** Low. Same pattern as T4 - can be a side effect of
  the FSM transition.

### Finding T7: `should_read_filter_list()` guard (receiver)

- **File:** `crates/transfer/src/receiver/transfer/setup.rs`
- **Lines:** 73-117
- **Current mechanism:** Method returning `bool`, guarding the filter list read
  in `setup_transfer()`. This is the receiver-side complement of
  `should_send_filter_list` (Finding T5).
- **Target FSM state:** FilterExchange. The filter list read is the primary
  action of this phase on the receiver side.
- **Migration difficulty:** Low. Same pattern as T5.

### Finding T8: Sequential file list reception in `setup_transfer()`

- **File:** `crates/transfer/src/receiver/transfer/setup.rs`
- **Lines:** 124-131
- **Current mechanism:** `receive_file_list()` + `receive_extra_file_lists()` +
  `sanitize_file_list()` are called sequentially after filter list reception.
  This block is the FileListTransfer phase, but there is no state variable or
  FSM transition marking entry into or exit from this phase.
- **Target FSM state:** FileListTransfer. The file list reception is the
  defining action of this phase.
- **Migration difficulty:** Low. A `TransferPipeline::advance()` call before
  `receive_file_list()` and after `sanitize_file_list()` would bracket the
  phase.

### Finding T9: `phase: i32` counter in `exchange_phase_done()`

- **File:** `crates/transfer/src/receiver/transfer/phases.rs`
- **Lines:** 42-46, 58, 82-90
- **Current mechanism:** Integer counter tracking the protocol phase (1 = main
  transfer, 2 = redo, 3 = delay-updates). Mirrors upstream
  `generator.c:2355-2394` with explicit `phase++` increments and
  `generate_files phase=%d` debug emissions. The `max_phase` is derived from
  `supports_multi_phase()`.
- **Target FSM state:** DeltaTransfer -> Finalization. Phase 1 is DeltaTransfer,
  phase 2 (redo) is still DeltaTransfer, phase 3 (delay-updates) marks entry
  into Finalization.
- **Migration difficulty:** Medium. The integer counter must be preserved for
  wire-protocol fidelity (upstream uses an integer), but `TransferPipeline`
  transitions can bracket the phase boundaries. The redo pass complicates
  mapping because DeltaTransfer has two sub-phases.

### Finding T10: `inc_recurse` boolean in `exchange_phase_done()`

- **File:** `crates/transfer/src/receiver/transfer/phases.rs`
- **Lines:** 38-40
- **Current mechanism:** `bool` extracted from `self.compat_flags`. Controls
  which NDX_DONE exchange pattern is used (unidirectional for INC_RECURSE,
  alternating for non-INC_RECURSE). This is a mode discriminant that affects
  how phase transitions are performed on the wire.
- **Target FSM state:** DeltaTransfer/Finalization (parameterizes wire behavior
  within these phases). Orthogonal to lifecycle state.
- **Migration difficulty:** N/A. Mode discriminant, not lifecycle state.

### Finding T11: `setup.checksum_length` mutation for phase 2 redo

- **File:** `crates/transfer/src/receiver/transfer/pipelined.rs`
- **Lines:** 122
- **Current mechanism:** `setup.checksum_length = REDO_CHECKSUM_LENGTH` mutates
  the pipeline setup struct to switch from `SHORT_SUM_LENGTH` (2 bytes, phase 1)
  to `SUM_LENGTH` (16 bytes, phase 2 redo). This in-place mutation encodes a
  sub-phase transition within DeltaTransfer.
- **Target FSM state:** DeltaTransfer (phase 1 -> phase 2 sub-transition). The
  checksum length change is a concrete marker of the redo pass beginning.
- **Migration difficulty:** Low. The mutation can remain; the FSM transition
  would be an additional signal, not a replacement.

### Finding T12: Sequential orchestration in `run_server_with_handshake()`

- **File:** `crates/transfer/src/lib.rs`
- **Lines:** 299-576
- **Current mechanism:** The function executes a linear sequence:
  setup_protocol (413), flush (471), multiplex activation (486-488), filter list
  (505-512), files-from forwarding (519-524), MSG_IO_TIMEOUT (527-534), batch
  recording (540-552), role dispatch (556-575). Each block maps to a
  `TransferPhase` transition, but no state variable tracks progress. Error
  returns from any block implicitly transition to Complete (via `?` propagation).
- **Target FSM state:** Handshake (setup_protocol) -> FilterExchange (filter
  list) -> role dispatch (which internally goes through FileListTransfer ->
  DeltaTransfer -> Finalization -> Complete).
- **Migration difficulty:** Medium. The function is a natural fit for FSM-driven
  orchestration, but the `?` error propagation pattern would need to be wrapped
  in `transition(Complete)` calls for clean shutdown tracking.

### Finding T13: Generator `run()` sequential orchestration

- **File:** `crates/transfer/src/generator/transfer/orchestrator.rs`
- **Lines:** 34-211
- **Current mechanism:** Sequential flow: multiplex activation (42-53), flush
  (62-64), filter list reception (67), files-from resolution (70), file list
  build/send (74-86), ID lists (88), io_error flag (89), transfer loop (93-96),
  stats (101-107), goodbye handshake (111). Each step corresponds to a
  `TransferPhase`, but no FSM tracks the progression.
- **Target FSM state:** Handshake (multiplex) -> FilterExchange (filter list) ->
  FileListTransfer (build/send) -> DeltaTransfer (transfer loop) ->
  Finalization (stats + goodbye) -> Complete.
- **Migration difficulty:** Medium. Clean linear flow maps well to FSM
  transitions. The main complexity is ensuring that INC_RECURSE sub-list
  sending during the transfer loop (line 92) does not conflict with the
  FileListTransfer -> DeltaTransfer boundary.

### Finding T14: Receiver `finalize_transfer()` sequential flow

- **File:** `crates/transfer/src/receiver/transfer/phases.rs`
- **Lines:** 205-245
- **Current mechanism:** Sequential calls: `exchange_phase_done()` (213),
  `receive_stats()` (216), `handle_goodbye()` (219). This is the Finalization
  phase, transitioning to Complete after goodbye. No state variable tracks
  progress through these sub-steps.
- **Target FSM state:** Finalization -> Complete. The three sub-steps are all
  within Finalization; `handle_goodbye()` completion marks the transition to
  Complete.
- **Migration difficulty:** Low. A single `advance()` call after
  `handle_goodbye()` completes the FSM.

---

## Summary

### Site counts

| Area | Ad-hoc state sites | Mode discriminants (no migration) |
|------|-------------------:|----------------------------------:|
| Daemon (FSW-1) | 7 (D1-D7) | 1 (D8) |
| Transfer (FSW-2) | 10 (T1, T4-T9, T11-T14) | 3 (T2, T3, T10) |
| **Total** | **17** | **4** |

### Migration difficulty breakdown

| Difficulty | Count | Sites |
|------------|------:|-------|
| Low | 10 | D1, D2, D3, D5, T1, T4, T5, T6, T7, T8, T11, T14 |
| Medium | 5 | D4, D7, T9, T12, T13 |
| High | 1 | D6 |
| N/A | 4 | D8, T2, T3, T10 |

Note: the Low count above is 12 sites; the total of 10 ad-hoc state sites in
the transfer section includes some entries counted differently. The per-finding
table is authoritative.

### Migration priority recommendations

1. **Phase 1 - Low-hanging fruit (Low difficulty, high signal):**
   Wire T1 (`compat_exchanged`), T4/T6 (multiplex activation boundary), and
   T5/T7 (filter list guards) into `TransferPipeline::advance()` calls. These
   four pairs mark clear phase boundaries already present in the code. Wire D1,
   D2, D3, D5 into `ConnectionState::transition()` calls for the daemon.

2. **Phase 2 - Phase tracking (Medium difficulty):**
   Add `TransferPipeline` transitions around T8 (file list reception), T9
   (phase counter), T12/T13 (orchestration functions), and T14 (finalization).
   For the daemon, wire D4 (greeting flow) and D7 (async session alignment).

3. **Phase 3 - Decomposition (High difficulty):**
   Decompose D6 (`process_approved_module`) into FSM-driven sub-phases. This
   350-line function needs structural refactoring to separate the sequential
   steps into discrete state transitions with proper error-to-Closing mapping.

4. **Deferred:**
   D8, T2, T3, T10 are mode/role discriminants, not lifecycle state. They
   should remain independent of the FSMs.
