# FSM Invalid and Unreachable State Transitions (FSM-3)

Audit of the daemon connection and transfer pipeline state machines
against the actual call graph, identifying dead transitions, unguarded
error jumps, and tightening opportunities.

## 1. Daemon Connection FSM

### 1.1 Specified States

The architecture describes five daemon connection states:

```
Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing
```

The `SessionState` enum in `daemon/session_registry.rs` has six
variants: `Handshaking`, `Authenticating`, `Listing`, `Transferring`,
`Completed`, `Failed`.

### 1.2 Actual Call Graph

The connection lifecycle in `handle_session` through
`handle_legacy_session` follows this path:

```
Accept -> configure_stream -> [PROXY header] -> resolve_hostname
  -> send greeting (cached_legacy_daemon_greeting)
  -> read client version + module name
  -> dispatch:
       "#list"  -> advertise_capabilities -> respond_with_module_list -> EXIT
       ""       -> send error -> EXIT
       <module> -> respond_with_module_request -> ...
```

`respond_with_module_request` dispatches through `process_approved_module`:

```
  module lookup (unknown? -> @ERROR -> EXIT)
  -> apply bandwidth limit
  -> host ACL check (denied? -> @ERROR -> EXIT)
  -> try_acquire_connection (limit? -> @ERROR -> EXIT; lock error? -> @ERROR -> EXIT)
  -> refused options check (refused? -> @ERROR -> EXIT)
  -> daemon param overrides
  -> authentication (denied? -> @ERROR -> EXIT)
  -> early exec (failed? -> @ERROR -> EXIT)
  -> read client args
  -> read-only / write-only check (violation? -> @ERROR -> EXIT)
  -> validate module path
  -> validate client paths in module
  -> chroot + privilege drop
  -> name converter spawn
  -> build server config
  -> daemon filter rules
  -> Landlock sandbox
  -> setup TCP streams
  -> pre-xfer exec (failed? -> @ERROR -> EXIT)
  -> build HandshakeResult
  -> execute_transfer (run_server_with_handshake)
  -> TCP shutdown
  -> post-xfer exec
```

### 1.3 Valid Transitions Never Exercised

| Transition | Why Unreachable |
|---|---|
| Greeting -> Closing (clean) | A client that sends `@RSYNCD: EXIT` during the greeting loop triggers `return Ok(())` at line 253 of `session_runtime.rs`. The FSM specification does not model this - it implies the greeting must always reach ModuleSelect. In practice this path is exercised by health-check probes. |
| ModuleSelect -> Closing (no auth) | When the client requests `#list`, the session never enters ModuleSelect in the FSM sense - it writes the module list and exits. The FSM specification models this as a separate `Listing` state only in `SessionState`, not in the five-state FSM. |
| Binary negotiation path | `SessionStyle::Binary` is defined but `#[allow(dead_code)]` - daemon mode always uses Legacy. The binary session handler sends a `HANDSHAKE_ERROR_PAYLOAD` and exits. No Binary -> Authenticating transition ever fires. |
| Authenticating -> ModuleSelect (re-select) | Upstream rsync allows a client to request a different module after authentication failure (the server loops back to reading a new module name). The oc-rsync implementation does not loop - authentication failure sends `@ERROR` and exits. This is a behavioral divergence from upstream `clientserver.c` which re-enters the module-request loop. |

### 1.4 Error Paths Causing Unexpected State Jumps

**Greeting -> Closing (implicit).** Any I/O error during the greeting
write (`cached_legacy_daemon_greeting`) propagates as `Err` from
`handle_legacy_session`, which causes the worker thread to exit. The
`SessionState` registry (if active) never transitions through
`Completed` or `Failed` - the thread simply terminates. This is benign
because the registry is observability-only, but the `SessionState` enum
would report a stale `Handshaking` state for the session until reaped.

**Authentication -> Closing (mid-stream).** If `read_trimmed_line`
returns `Err` during `perform_module_authentication` (e.g., the client
disconnects mid-challenge), the error propagates through
`handle_authentication` -> `process_approved_module` ->
`respond_with_module_request` -> `handle_legacy_session`. The connection
closes without sending `@ERROR: auth failed`. This is correct behavior
(no point writing to a dead socket), but the FSM has no explicit
"error during authentication" transition.

**Transfer -> Closing (panic).** `spawn_connection_worker` wraps
`handle_session` in `catch_unwind`. A panic during transfer (e.g., in
`run_server_with_handshake`) is caught, logged, and the thread exits
with `Ok(())`. The daemon survives, but the `SessionState` is never set
to `Failed`. The client sees a RST or broken pipe.

**PROXY header parse failure.** If PROXY protocol parsing fails, the
session exits before the greeting is sent. This pre-Greeting error has
no representation in the five-state FSM.

### 1.5 Transitions That Should Be Blocked But Are Not

**ModuleSelect -> Transferring (bypassing auth).** The code correctly
checks `module.requires_authentication()` and only calls
`send_daemon_ok` without authentication when auth is not required. This
is intentional and matches upstream. No unguarded bypass exists.

**Greeting version mismatch acceptance.** The daemon records
`negotiated_protocol` from the client's `@RSYNCD: <version>` line but
does not reject unsupported versions during the greeting phase. Version
validation happens later in `setup_protocol` via
`ProtocolVersion::from_peer_advertisement`. A client advertising
protocol 99 would pass the greeting and module-select phases before
failing during transfer setup. The FSM should ideally block this at the
Greeting -> ModuleSelect transition.

## 2. Transfer Pipeline FSM

### 2.1 Specified States

The architecture describes six transfer states:

```
Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer -> Finalization -> Complete
```

The `ProtocolState<P>` typestate in `protocol/src/state/typestate.rs`
models four phases: `Negotiation -> FileList -> Transfer -> Finalize`.

The `DynamicProtocolState` in `protocol/src/state/dynamic.rs` models the
same four phases as a runtime enum.

### 2.2 Actual Call Graph (Receiver)

```
run_server_with_handshake:
  1. setup_protocol (compat flags, capability negotiation, checksum seed)
  2. Flush raw-mode output
  3. Activate output multiplex
  4. Write filter list (client mode, if applicable)
  5. Write files-from data (client mode pull)
  6. Send MSG_IO_TIMEOUT (server mode)
  7. Activate batch recording
  8. Dispatch to ReceiverContext::run or GeneratorContext::run

ReceiverContext::run -> run_pipelined_incremental:
  1. setup_transfer:
     a. Activate input multiplex
     b. Read filter list (if server mode)
     c. Receive file list (+ extra INC_RECURSE segments)
     d. Sanitize file list
     e. Build checksum factory + metadata options
  2. Create directories (incremental)
  3. Create symlinks
  4. Build files_to_transfer list
  5. run_pipeline_loop_decoupled (phase 1: SHORT_SUM_LENGTH)
  6. run_pipeline_loop_decoupled (phase 2: MAX_SUM_LENGTH, redo only)
  7. Create hardlinks
  8. finalize_transfer:
     a. exchange_phase_done (NDX_DONE ping-pong)
     b. receive_stats (client mode)
     c. handle_goodbye (NDX_DONE echo for protocol >= 31)
```

### 2.3 Actual Call Graph (Generator/Sender)

```
GeneratorContext::run:
  1. Activate input multiplex
  2. Flush output (server mode)
  3. Receive filter list (if server)
  4. Resolve files-from paths
  5. Build file list
  6. Partition file list for INC_RECURSE
  7. Send file list
  8. Send ID lists (uid/gid mappings)
  9. Send io_error flag
  10. run_transfer_loop (send deltas in response to NDX requests)
  11. Send stats (server mode)
  12. handle_goodbye
```

### 2.4 Valid Transitions Never Exercised

| Transition | Why Unreachable |
|---|---|
| Handshake -> Complete (empty transfer) | A dry-run with zero files still walks through FilterExchange, FileListTransfer (sending an empty list), DeltaTransfer (`run_dry_run_loop` with no files), and Finalization. No short-circuit path exists. |
| FilterExchange -> Complete (filter error) | Filter list parse errors in `parse_wire_filters_for_receiver` propagate as `Err`, unwinding through `setup_transfer` to `run_pipelined_incremental` to `run_server_with_handshake`. The transfer never reaches a `Complete` state - it returns `Err` to the caller. The FSM has no explicit error-exit transition from FilterExchange. |
| FileListTransfer -> Complete (flist error) | Same pattern. If `receive_file_list` fails, the error propagates upward. No explicit FSM transition covers this. |
| DeltaTransfer -> FilterExchange (re-negotiate) | The FSM is strictly linear. There is no backward transition in the code or specification. |

### 2.5 Error Paths Causing Unexpected State Jumps

**Multiplex activation failure.** Both receiver and generator call
`reader.activate_multiplex()` as the first step of their `run` method.
If this fails, execution jumps from Handshake directly to an error
return. The `ProtocolState<Negotiation>` typestate would still be in the
Negotiation phase - it was never transitioned. The six-state FSM has no
explicit "multiplex activation" step.

**Phase-done exchange failure.** During `exchange_phase_done`, if the
sender sends an unexpected NDX value instead of `NDX_DONE`, the receiver
returns `Err` with `"expected NDX_DONE (-1) from sender"`. This jumps
from Finalization to an error exit without passing through Complete. The
typestate `ProtocolState<Finalize>` has a `summarize()` consuming
method, but the error path never calls it.

**Stats reception failure.** In `finalize_transfer`, if
`receive_stats` fails (truncated stream), the error propagates before
`handle_goodbye` runs. The goodbye handshake is skipped entirely. In
upstream rsync, this would cause the client to wait indefinitely for the
goodbye NDX_DONE. The FSM has no transition for "stats failed, skip
goodbye".

**INC_RECURSE segment mismatch.** In `exchange_phase_done`, the number
of NDX_DONE messages sent depends on `self.ndx_segments.len()`. If the
sender and receiver disagree on the segment count (e.g., due to a
partial file list under INC_RECURSE), the receiver will either send too
few NDX_DONE messages (causing the sender to hang) or read an unexpected
NDX from the sender (causing an error). Neither case has explicit FSM
handling.

**Redo phase skip.** When `redo_indices` is empty after phase 1, the
code skips the second `run_pipeline_loop_decoupled` call entirely. The
FSM specification says DeltaTransfer -> Finalization, but the redo phase
is an implicit sub-state within DeltaTransfer that the FSM does not
model. This is correct behavior but is invisible to the state trackers.

### 2.6 Transitions That Should Be Blocked But Are Not

**run_server_with_handshake allows re-entry.** The function is stateless
- it takes `ServerConfig` and `HandshakeResult` by value and can be
called multiple times on the same TCP streams. Nothing prevents a caller
from invoking it twice, which would send a second protocol negotiation
over an already-negotiated connection. The `ProtocolState` typestate
consumes `self` on each transition (preventing re-use), but
`run_server_with_handshake` does not use the typestate tracker - the
transfer phases are enforced by linear function-call ordering, not by
the type system.

**setup_protocol can run without multiplex.** The compat-flags exchange
in `setup_protocol` happens in raw mode before multiplex activation. If
a caller somehow activated multiplex before calling `setup_protocol`,
the raw bytes would be wrapped in multiplex frames, corrupting the
negotiation. No guard prevents this ordering.

**Filter list sent after multiplex but before transfer.** In
`run_server_with_handshake`, the filter list is written after output
multiplex activation (line 503) but the filter list wire format uses raw
bytes, not MSG_DATA frames. This works because `write_filter_list` uses
the `ServerWriter` which routes through the multiplex layer. If a caller
bypassed the writer and wrote directly to the underlying stream, the
filter list would not be framed. This is not a bug in the current code
but the FSM does not enforce that filter list writing must use the
multiplexed writer.

## 3. Gaps Between FSM Specifications and Existing State Trackers

| FSM Specification | Existing Tracker | Gap |
|---|---|---|
| Daemon: Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing | `SessionState` enum (6 variants) | `SessionState` is observability-only - no transition validation. The daemon code never writes to the registry (it requires the `concurrent-sessions` feature and the registry is not wired into the session handler). The five-state FSM omits `Listing`. |
| Transfer: 6 states | `ProtocolState<P>` typestate (4 phases) | The typestate merges FilterExchange into Negotiation and does not distinguish DeltaTransfer from FileListTransfer as separate phases. The typestate is a tracking shell - `run_server_with_handshake` does not use it to guard transitions. |
| Transfer: 6 states | `DynamicProtocolState` (4 phases, runtime) | Same four-phase model. `advance()` validates prerequisites but is not called by the transfer pipeline - it exists for downstream embedders. |

## 4. Recommendations

### 4.1 Daemon Connection FSM

1. **Model the `#list` path.** Add a `Listing` state to the five-state
   FSM specification. The code already handles it as a distinct branch.

2. **Model early exit.** Add `Greeting -> Closing` for `@RSYNCD: EXIT`
   during the greeting loop, and `Accept -> Closing` for PROXY header
   failures. Both are valid production paths.

3. **Block unsupported protocol versions at greeting.** Reject protocol
   versions outside the supported range (28-32) before entering
   ModuleSelect, rather than deferring to `setup_protocol`. This would
   prevent unnecessary module lookup and authentication for clients that
   will fail anyway.

4. **Wire `SessionState` into the session handler.** If the
   `concurrent-sessions` feature is active, the session handler should
   update the registry at each phase transition. Currently the registry
   is populated at session creation but never updated.

5. **Consider upstream's module-reselection loop.** Upstream rsync
   loops back to read a new module name after authentication failure.
   Document the divergence or implement the loop if interop requires it.

### 4.2 Transfer Pipeline FSM

1. **Add error-exit transitions.** Every phase should have an explicit
   transition to an `Error` terminal state. The current FSM only models
   the happy path. The `ProtocolState<Finalize>::summarize()` method is
   the closest thing to a terminal transition, but error paths never
   reach it.

2. **Model the redo sub-phase.** DeltaTransfer contains an implicit
   phase 1 / phase 2 boundary (SHORT_SUM_LENGTH vs MAX_SUM_LENGTH).
   Make this explicit in the FSM specification as a self-transition or
   a sub-state.

3. **Guard multiplex ordering.** Add a `multiplex_activated` flag to the
   transfer context that `setup_protocol` checks. If multiplex is
   already active when `setup_protocol` runs, return an error. This
   prevents the wire-corruption scenario described in section 2.6.

4. **Use the typestate tracker in production.** The `ProtocolState<P>`
   typestate exists but is not used by `run_server_with_handshake`. Wire
   it in so that `begin_file_list()` is called before file list I/O and
   `begin_transfer()` before delta I/O. This would catch phase-ordering
   bugs at compile time instead of relying on function-call ordering.

5. **Align phase counts.** The FSM specification has 6 states but the
   typestate and dynamic trackers have 4 phases. Either split the
   typestate's `Negotiation` into `Handshake` + `FilterExchange`, or
   update the specification to use 4 phases. The current mismatch makes
   it unclear which is the source of truth.
