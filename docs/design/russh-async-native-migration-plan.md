# russh async-native migration plan (RUSSH-ASY series)

**Tracking:** RUSSH-ASY.1 (#3989), RUSSH-ASY.2 (#3990)
**Status:** Audit deliverable. Inventories every `tokio::spawn_blocking` site at the russh -> sync boundary, ranks them by saturation impact, and pre-commits the migration order for RUSSH-ASY.3+.
**Predecessors:** RUSSH-1 (`docs/audit/russh-spawn-blocking-ceiling-inventory.md`, #2804), RUSSH-9 (`docs/design/russh-async-native-path.md`, #2812), RUSSH-10 (`docs/design/russh-async-native-back-compat-shim.md`, #2813), RUSSH-14 (`docs/design/russh-spawn-blocking-decision.md`).
**Scope:** Production call sites in `crates/rsync_io/src/`, `crates/transport/src/`, `crates/core/src/client/remote/`, and the engine async copier path that pairs with the russh transport. Daemon hybrid listener is included for budget accounting because it competes for the same blocking pool.

Cross-links: [[project_russh_spawn_blocking_ceiling]], [[project_no_async_threaded_only]], [[project_ssh_push_russh_v062]], [[project_russh_async_native_back_compat_shim]].

## 1. Current state - inventory (RUSSH-ASY.1)

### 1.1 Production call sites

The grep across `crates/rsync_io/src/`, `crates/transport/src/`, and `crates/core/src/client/remote/` returns three production call sites and one off-russh-but-runtime-shared site. The synchronous bridge module under `crates/rsync_io/src/ssh/embedded/sync_bridge.rs` uses `tokio::spawn` (not `spawn_blocking`) for its background pump and is therefore counted separately as non-blocking-pool traffic.

| # | File:Line | Function | Closure body | Work classification | Frequency |
|---|-----------|----------|--------------|---------------------|-----------|
| 1 | `crates/core/src/client/remote/async_ssh_transport.rs:349` | `run_async_session::writer_fanin` | Drain sync `std_mpsc::Receiver<Vec<u8>>` and forward each chunk via `tokio_mpsc::Sender::blocking_send`. Bridges the sync `SyncWriter` chunks to the async outbound pump. | I/O-bound. The blocking work is `recv()` on a sync queue plus `blocking_send` on a tokio queue; both are channel primitives that have async-native equivalents (`tokio::sync::mpsc` recv + send). No CPU work. | Per session, long-lived (full session lifetime). |
| 2 | `crates/core/src/client/remote/async_ssh_transport.rs:361` | `run_async_session::server_handle` | Runs `run_blocking_server`: full sync transfer pipeline (handshake, file list, delta apply, finalize). | Mixed. The transfer pipeline is intentionally threaded (`[[project_no_async_threaded_only]]`); it is not a candidate for async-native rewrite. The `spawn_blocking` wrapper is the bridge into the tokio runtime, not the pipeline itself. The wrapper is replaceable with `std::thread::spawn` + a one-shot `tokio::sync::oneshot` for the join, which gets us off the blocking pool without touching pipeline code. | Per session, long-lived (full session lifetime). |
| 3 | `crates/engine/src/async_io/copier.rs:184` | `AsyncCopier::copy_file` | Apply `set_permissions` + `set_file_mtime` after each async copy completes. | I/O-bound. Two stat-class syscalls per file. tokio 1.x exposes `tokio::fs::set_permissions` (async-native) and `filetime` has no async wrapper; a small async helper around a blocking `fs` call sized at the file granularity is acceptable. | Per file when `-p`/`-t` set; transient. |
| 4 | `crates/daemon/src/async_listener.rs:133` | `accept_loop` inner spawn | Dispatch each accepted std-TCP stream to the daemon `SyncWorker`. | The wrapped work is the full daemon session (handshake, auth, transfer); same shape as #2. | Per daemon connection, long-lived (full session lifetime). |

`crates/transport/src/` returns zero hits. There is no `spawn_blocking` in the SSH-side `ssh_transfer.rs` path either - it routes through `async_ssh_transport.rs` (sites #1 and #2 above).

### 1.2 Doc-only and test-only references (not on the production ceiling)

The remaining grep hits are not blocking-pool consumers and are listed here so they are not mistaken for production sites in future audits:

- `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:297, 528, 613` - rustdoc strings and comments referring to the bridge contract. The bridge itself uses `tokio::spawn` at `:331`, not `spawn_blocking`. Not on the blocking pool.
- `crates/transfer/src/receiver/directory/{creation,deletion}.rs`, `crates/transfer/src/parallel_io.rs` - rustdoc strings that still mention `spawn_blocking` even though the implementations now use rayon. Documentation drift; off the russh boundary; flagged in section 5 below.
- `crates/rsync_io/tests/concurrent_session_validation.rs`, `crates/rsync_io/benches/concurrent_session_scaling.rs`, `crates/rsync_io/benches/ssh_sync_vs_async.rs` - bench/test harnesses for the existing bridge. Not production.
- `crates/daemon/src/async_listener.rs:218` - test comment about drain timing.

### 1.3 Per-session blocking-pool cost

The async-SSH client path reserves 2 long-lived slots per session (sites #1 and #2) plus `0..N` transient slots from site #3 when metadata preservation is on. The daemon hybrid listener adds 1 long-lived slot per accepted connection (site #4).

The combined ceiling for a single process running both async-SSH client transfers and the hybrid daemon listener is approximately `max_blocking_threads / 3` long-lived slots, before per-file metadata bursts erode any remaining headroom.

## 2. Cross-reference with RUSSH-1 (#2804)

RUSSH-1 inventoried the same surface at audit time (`docs/audit/russh-spawn-blocking-ceiling-inventory.md`). The diff between RUSSH-1 and RUSSH-ASY.1:

| Site | RUSSH-1 (audit) | RUSSH-ASY.1 (this audit) | Change |
|------|-----------------|---------------------------|--------|
| `async_ssh_transport.rs:349` (`writer_fanin`) | Listed | Listed | None. |
| `async_ssh_transport.rs:361` (`server_handle`) | Listed | Listed | None. |
| `daemon/src/async_listener.rs:133` (hybrid dispatch) | Listed | Listed | None. |
| `engine/src/async_io/copier.rs:184` (metadata) | Listed | Listed | None. |
| `crates/transport/src/` | Not listed | Not listed | Still zero. |
| `crates/core/src/client/remote/ssh_transfer.rs` | Not listed | Not listed | Confirmed: routes to the async transport site #1/#2 path; no separate `spawn_blocking`. |

No production sites have been added or removed since RUSSH-1. The priority ranking has not shifted: the two long-lived per-session sites (#1, #2) dominate the ceiling; the per-file site (#3) is a multiplier; the daemon hybrid (#4) is structurally the same as #2.

The async-native path design (RUSSH-9, `docs/design/russh-async-native-path.md`) already targets sites #1 and #2 as the primary replacement scope and leaves #3 and #4 for follow-up. RUSSH-ASY.1 confirms this scope is still correct.

## 3. Saturation analysis - priority ranking (RUSSH-ASY.2)

| Rank | Site | File:Line | Frequency | Saturation impact | Suitable for async-native |
|------|------|-----------|-----------|-------------------|---------------------------|
| 1 | `server_handle` (sync transfer pipeline wrapper) | `crates/core/src/client/remote/async_ssh_transport.rs:361` | Per session, long-lived (full session) | HIGH. One of two long-lived blocking-pool slots per async-SSH session. At default `max_blocking_threads = 512` this is the binding factor: the slot is held from handshake through finalize. RUSSH-5/6/7 at 128/256/512 concurrent sessions saturate the pool here first. | Yes via mechanical swap. Replace `tokio::task::spawn_blocking` with `std::thread::spawn` + `tokio::sync::oneshot` for the join. The wrapped pipeline stays threaded per `[[project_no_async_threaded_only]]`. Removes one blocking-pool slot per session without touching the transfer engine. |
| 2 | `writer_fanin` (sync->async write pump) | `crates/core/src/client/remote/async_ssh_transport.rs:349` | Per session, long-lived (full session) | HIGH. The second of the two long-lived blocking-pool slots per session. Combined with rank 1 these define the ~256-session ceiling per process at tokio defaults. | Yes, native. Replace the `std::sync::mpsc` outbound channel with `tokio::sync::mpsc` end-to-end and delete the fan-in pump entirely. The sync transfer thread writes through a tokio queue directly; no second async hop required. |
| 3 | `hybrid daemon dispatch` (per-connection daemon worker) | `crates/daemon/src/async_listener.rs:133` | Per connection, long-lived (full daemon session) | MEDIUM-HIGH. Same shape as rank 1 but only relevant when the async-daemon feature is on. Same fix applies: `std::thread::spawn` + `oneshot` join. Independent rollout because the feature gate is different (`async-daemon` vs `async-ssh`). | Yes via the same mechanical swap as rank 1. |
| 4 | `metadata application` (per-file `set_permissions`/`set_file_mtime`) | `crates/engine/src/async_io/copier.rs:184` | Per file (transient, only when `-p`/`-t`) | LOW per session, but a multiplier that erodes the ceiling on large directory transfers with metadata preservation. Each transient slot is short, but at high session counts and high file counts the burst is observable. | Optional. tokio has `tokio::fs::set_permissions` (native async). `filetime::set_file_mtime` has no native async; a small `tokio::task::spawn_blocking` per file is acceptable here because the work is genuinely a single syscall. Migrating site #4 is lowest ROI and should be the last step. |

Rationale for the ranking:

- Sites #1 and #2 are equal in saturation impact (both per-session, both full-session-lifetime, both long-lived blocking-pool slots). They are ranked 1 and 2 because #1 is mechanically simpler (drop-in replacement) while #2 requires changing the channel type from `std::sync::mpsc` to `tokio::sync::mpsc` throughout the bridge - higher engineering scope. Implementation order should respect the difficulty gradient, not the saturation impact.
- Site #4 (daemon hybrid) is structurally identical to site #1 but is gated on a different feature flag and rolls out separately. It is ranked third because the async-daemon feature is not the default and the saturation pressure is lower in current deployments.
- Site #3 (per-file metadata) is genuinely I/O-bound on a single syscall scale; the cost-of-migration vs cost-of-keep tradeoff favors keeping `spawn_blocking` here unless the rest of the migration is done and #3 becomes the last hot spot.

## 4. Migration order (recommended for RUSSH-ASY.3+)

The migration order pre-commits to the saturation ranking above, modified by implementation difficulty:

1. **RUSSH-ASY.3 (highest priority): replace site #1 (`server_handle`) with `std::thread::spawn` + `tokio::sync::oneshot`.** Single function change in `run_async_session`. The blocking work stays on its own OS thread; the join becomes async-native. Estimated diff: 30 LoC, one file. Bench impact: removes one of the two long-lived blocking-pool slots per session.
2. **RUSSH-ASY.4: replace site #2 (`writer_fanin`) by switching the outbound channel from `std::sync::mpsc` to `tokio::sync::mpsc`.** This deletes the fan-in pump entirely. Scope: ~80 LoC across `run_async_session` and the `SyncWriter` channel type. Bench impact: removes the second long-lived blocking-pool slot per session. Combined with RUSSH-ASY.3 this reduces per-session blocking-pool cost from 2 slots to 0 for the steady-state transfer.
3. **RUSSH-ASY.5: re-bench at 128 / 256 / 512 / 1024 concurrent sessions.** Reuse the RUSSH-3 harness. Compare against the `spawn_blocking` baseline already collected under RUSSH-4..8. Confirm: per-session throughput within +/- 5% of baseline; blocking-pool slot count drops to zero for the steady-state; OS thread count rises by one per session (acceptable). The decision criteria are pre-committed in RUSSH-14 (`docs/design/russh-spawn-blocking-decision.md`).
4. **RUSSH-ASY.6: flip the default.** If RUSSH-ASY.5 meets the criteria, swap the env-var default from `OC_RSYNC_SSH_DISPATCH=spawn_blocking` to `OC_RSYNC_SSH_DISPATCH=async_native`. Keep the env var for rollback.
5. **RUSSH-ASY.7 (optional, low ROI): migrate site #4 (hybrid daemon).** Same mechanical change as RUSSH-ASY.3 applied to `accept_loop`. Gated behind the `async-daemon` feature.
6. **RUSSH-ASY.8 (optional, lowest ROI): migrate site #3 (per-file metadata).** Switch to `tokio::fs::set_permissions`; keep a one-syscall `spawn_blocking` for `filetime::set_file_mtime`. Defer until RUSSH-ASY.7 is in place and bench evidence shows site #3 as the next hot spot.

Each step is independently revertible by toggling the dispatch env var or by reverting the single-file diff. No step requires a wire-format change; the russh boundary is internal.

## 5. Rejected alternatives

- **Removing russh entirely.** Would lose the async ecosystem benefits (rust-native handshake, ed25519 / chacha20 in safe Rust, tokio integration with the existing daemon listener path). Rejected.
- **Replacing russh with sync `ssh2`.** Would add a separate sync C dependency (libssh2), contradict the move toward async-native I/O, and reintroduce the FFI surface the v0.6.2 migration moved away from. Rejected.
- **Raising `max_blocking_threads` past 512.** Reactive, not proactive. The blocking-pool slot is a `pthread` with its own stack; raising the cap pushes the saturation point higher but does not change the structural cost per session. Rejected as a primary fix; acceptable as a short-term operational lever while the migration is in flight.
- **Switching the whole transfer pipeline to async/await.** Explicitly off the table per `[[project_no_async_threaded_only]]`. The pipeline stays threaded; only the russh boundary moves async-native.
- **Per-process dedicated blocking pool with a separate cap.** Would isolate russh's blocking-pool consumption from the rest of the runtime but requires a custom tokio runtime build and adds operational complexity. Rejected because the async-native migration is simpler and reaches a better steady state (zero blocking-pool slots per session).

## 6. Composition with ASY-G

The RUSSH-ASY migration overlaps with the ASY-G default-on flip for the broader `async` feature flag. The ordering constraint is:

1. RUSSH-ASY.3 and RUSSH-ASY.4 implement async-native paths behind the existing `russh-async-native` Cargo feature (already defined under RUSSH-9 / RUSSH-10).
2. RUSSH-ASY.5 confirms the wins at 128 / 256 / 512 sessions per the RUSSH-14 decision criteria.
3. RUSSH-ASY.6 flips russh to async-native default. The env var stays as the rollback lever.
4. Only after RUSSH-ASY.6 lands and is stable can ASY-G flip the workspace-level `async` feature default-on, because the workspace-level flip implicitly assumes the russh boundary is no longer a blocking-pool consumer.

If RUSSH-ASY.5 fails the bench criteria, the russh boundary stays on `spawn_blocking` by default. ASY-G can still flip independently for the parts of the pipeline that do not touch the russh boundary, but the russh boundary continues to consume blocking-pool slots until a follow-up RUSSH-ASY pass meets the criteria.

## 7. Documentation drift to clean up (out of scope, flagged)

The following rustdoc strings still reference `spawn_blocking` even though the underlying implementations have moved to rayon. Not part of RUSSH-ASY but worth a one-shot cleanup pass:

- `crates/transfer/src/receiver/directory/creation.rs:27`
- `crates/transfer/src/receiver/directory/deletion.rs:32`
- `crates/transfer/src/parallel_io.rs:6, 165`

These do not affect the ceiling and are off the russh boundary.

## 8. Follow-up tasks (recommended; not filed via tracker here)

The audit does not surface SPECIFIC actionable sites beyond what is already tracked under RUSSH-5/6/7/8/13 + ASY-7/8/10. The migration steps in section 4 are the actionable follow-ups; they map onto the existing RUSSH-ASY tracker numbering (.3 through .8). No new tracker entries are recommended.
