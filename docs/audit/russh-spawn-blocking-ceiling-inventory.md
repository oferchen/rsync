# russh `spawn_blocking` Call Sites + Tokio Runtime Sizing Inventory

**Tracking:** RUSSH-1 (#2804), RUSSH-2 (#2805)
**Status:** Audit-only inventory. Feeds RUSSH-3 bench harness design.
**Scope:** russh-adjacent SSH boundary across `crates/rsync_io/`, `crates/transport/`, `crates/engine/`, `crates/core/`, and the daemon hybrid-listener path in `crates/daemon/`.

The russh SSH boundary spans the sync transfer pipeline and an async tokio runtime. The bridge is implemented with `tokio::task::spawn_blocking` plus paired `std::sync::mpsc` / `tokio::sync::mpsc` queues. Because `spawn_blocking` parks on the tokio blocking pool (default cap 512 threads), the boundary inherits a hard ceiling that bounds concurrent SSH sessions per process. This document inventories every call site and the runtime sizing decisions that surround them, so RUSSH-3 can size the bench harness around the relevant chokepoints.

## Section 1 - `spawn_blocking` Call Sites (russh-adjacent)

Production call sites only. Test, bench, and doc-only references are listed at the end for completeness.

| # | File:Line | Closure purpose | Runtime context | Per-connection multiplicity |
|---|-----------|-----------------|-----------------|------------------------------|
| 1 | `crates/core/src/client/remote/async_ssh_transport.rs:349` | `writer_fanin`: drain sync `std_mpsc::Receiver` of outbound chunks and feed the async `tokio_mpsc::Sender` via `blocking_send`. Bridges the synchronous server's `SyncWriter` to the async writer pump. | `current_thread` tokio runtime built at line 245 (one per SSH session). | 1 per session. |
| 2 | `crates/core/src/client/remote/async_ssh_transport.rs:361` | `server_handle`: runs `run_blocking_server`, the full synchronous server transfer loop (handshake, file list, delta apply, finalize). Hosts the entire sync transfer pipeline for the duration of the session. | Same `current_thread` runtime as #1. | 1 per session, holds a blocking-pool slot for the full session lifetime. |
| 3 | `crates/daemon/src/async_listener.rs:133` | Hybrid daemon listener: dispatch each accepted `std::net::TcpStream` to the sync `SyncWorker` closure on the blocking pool. The sync worker runs the existing per-connection daemon session machinery. | `multi_thread` tokio runtime built at line 80 (process-wide accept runtime). | 1 per accepted daemon connection, held for the full session lifetime. |
| 4 | `crates/engine/src/async_io/copier.rs:184` | Apply post-copy filesystem metadata (`set_permissions`, `filetime::set_file_mtime`) outside the async runtime. Short-lived; not SSH-specific but on the async copy path that pairs with the russh transport. | Caller-supplied tokio runtime (typically the same `current_thread` runtime as #1 when reached via the async SSH path). | 1 per copied file when `preserve_permissions` or `preserve_timestamps` is set. |

**Reachability summary:** Items #1 and #2 are the russh / async-SSH boundary proper. Item #3 is the optional async-daemon listener that uses the same blocking-pool pattern and competes for the same pool budget when both are active in-process. Item #4 is engine-internal but consumes blocking-pool slots from the same runtime during async-SSH transfers, so it must be counted toward the ceiling.

**Per-session blocking-pool cost when async-SSH is engaged:**

- 1 long-lived slot for the sync server (item #2, full session lifetime).
- 1 long-lived slot for the writer fan-in pump (item #1, full session lifetime).
- 0..N transient slots for post-copy metadata application (item #4, per file).

The two long-lived slots per session make the ceiling roughly `max_blocking_threads / 2` concurrent SSH sessions, before the per-file metadata bursts erode headroom further.

**Test, bench, and doc-only references (not part of the production ceiling):**

- `crates/daemon/src/async_listener.rs:218` - test comment about drain timing, not a call.
- `crates/rsync_io/benches/ssh_sync_vs_async.rs:168`, `:211` - bench harness for the legacy `SshConnection` async shim.
- `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:246` - rustdoc note pointing at the public sync bridge contract; the bridge itself uses `tokio::spawn` (not `spawn_blocking`) at line 280.
- `crates/transfer/src/receiver/directory/creation.rs:26`, `crates/transfer/src/receiver/directory/deletion.rs:32`, `crates/transfer/src/parallel_io.rs:6,165` - historical or aspirational rustdoc strings; the parallel paths now use rayon, not `spawn_blocking`. Off the russh boundary.

## Section 2 - Tokio Runtime Sizing (russh-adjacent runtimes)

| # | File:Line | Builder flavor | `worker_threads` | `max_blocking_threads` | `thread_name` | Lifetime / scope |
|---|-----------|----------------|------------------|------------------------|---------------|------------------|
| 1 | `crates/core/src/client/remote/async_ssh_transport.rs:245` | `Builder::new_current_thread()` | n/a (single thread) | **unset -> tokio default 512** | unset | Per SSH session, dropped at `run_async_session` return. |
| 2 | `crates/rsync_io/src/ssh/embedded/connect.rs:177` | `Builder::new_current_thread()` | n/a | **unset -> default 512** | unset | Per `connect_and_exec` call; drives russh handshake / auth and emits the sync read/write halves. |
| 3 | `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:95` | `Builder::new_current_thread()` (inside `SyncAsyncBridge::new`) | n/a | **unset -> default 512** | unset | Per `SyncAsyncBridge` instance. Drives one async stream from sync callers. |
| 4 | `crates/daemon/src/async_listener.rs:80` (`run_hybrid_listener`) | `Builder::new_multi_thread()` with `.enable_io().enable_time().thread_name("oc-rsyncd-async")` | `worker_threads(worker_threads)` where caller passes `available_parallelism().min(8)` from `daemon.rs:233` | **unset -> default 512** | `"oc-rsyncd-async"` | Process-wide for the duration of the daemon; hosts every accepted connection through `spawn_blocking`. |
| 5 | `crates/rsync_io/src/ssh/async_transport.rs:345` | `Builder::new_current_thread()` | n/a | **unset -> default 512** | unset | Test helper `rt()`. Not on the production path; included for completeness. |
| 6 | `crates/rsync_io/src/ssh/async_stderr_drain.rs:277` | `Builder::new_current_thread()` | n/a | **unset -> default 512** | unset | Test helper `rt()`. Not on the production path. |
| 7 | `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:323` | `Builder::new_multi_thread()` | `worker_threads(2)` | **unset -> default 512** | unset | Test fixture `sync_async_bridge_round_trip`. |
| 8 | `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:421` | `Builder::new_multi_thread()` | `worker_threads(1)` | **unset -> default 512** | unset | Test fixture `sync_writer_backpressure_blocks_until_drained`. |
| 9 | `crates/rsync_io/src/ssh/embedded/sync_bridge.rs:460` | `Builder::new_current_thread()` | n/a | **unset -> default 512** | unset | Test fixture `dropping_sync_writer_closes_channel`. |
| 10 | `crates/daemon/src/daemon/async_session/mod.rs:77,96,110,147` | `tokio::runtime::Runtime::new()` (default multi-thread) | tokio default (CPU count) | **unset -> default 512** | unset | Test helpers (`#[cfg(test)]`) that drive async-session unit tests. |
| 11 | `crates/daemon/src/async_listener.rs:188` | `tokio::runtime::Runtime::new()` | tokio default | **unset -> default 512** | unset | Test fixture for `run_hybrid_listener` integration tests. |

**Workspace tokio pin:** `tokio = "1.52"` (workspace `Cargo.toml:226`) with features `rt-multi-thread, io-util, net, fs, sync, time, process, macros`. Tokio 1.x defaults `max_blocking_threads = 512` and `worker_threads = num_cpus` for `new_multi_thread()` unless explicitly overridden. Neither default is overridden anywhere on the russh boundary.

**Feature gating recap:**

- Async-SSH client path (#1, #2, #3): gated behind `async-ssh` in `crates/rsync_io` / `crates/core`. Opt-in via `OC_RSYNC_ASYNC_SSH=1` env var (see `ENV_OPT_IN`).
- Embedded russh path (#2): gated behind `embedded-ssh` in `crates/rsync_io` / `crates/core`.
- Hybrid daemon listener (#4): gated behind `async-daemon` in `crates/daemon`. Skeleton today; default `serve_connections` thread-per-connection loop is still in effect.

## Section 3 - Gap List (priority-ordered, audit-only)

Each gap is a question RUSSH-3 needs to answer or a constraint that should be benched.

### P0 - blocks the daemon-scale ceiling claim

1. **No explicit `max_blocking_threads` anywhere on the russh boundary.** Every runtime in Section 2 inherits tokio's default 512. With two long-lived slots per async-SSH session (items #1 and #2 in Section 1), the hard ceiling is roughly 256 concurrent sessions per process before `spawn_blocking` queues. RUSSH-3 should bench this exact threshold to confirm the failure mode is queueing, not panic / OOM, and measure the latency cliff as the pool saturates.
2. **Per-session `current_thread` runtimes (#1, #2, #3 in Section 2).** Every async-SSH session builds and drops its own runtime. At hundreds of sessions per second the construct/drop cost itself becomes measurable. RUSSH-3 should record runtime-build latency under the same load profile, and consider whether a single shared multi-thread runtime is a candidate for RUSSH-4.
3. **Daemon hybrid runtime caps `worker_threads` at 8** (`crates/daemon/src/daemon.rs:236`: `available_parallelism().min(8)`). The cap is fine for the accept loop, but every dispatched connection still grabs a 512-slot blocking pool. Bench needs to confirm the accept loop is not the bottleneck before the blocking pool is.

### P1 - amplifiers that erode the ceiling

4. **`engine::async_io::copier` `spawn_blocking` per file with metadata preservation** (item #4). On large directory transfers with `-p`/`-t`, the blocking pool is hit once per file in addition to the two session-level slots. RUSSH-3 should include a workload with many small files plus metadata flags so the per-file burst is visible.
5. **Daemon connection limiter is decoupled from the blocking pool.** `--max-connections` admission control (`crates/daemon/.../server_runtime/`) bounds session count but does not know about the tokio blocking-pool budget. If `max-connections > max_blocking_threads / 2`, the limiter happily admits sessions the runtime cannot service. Bench should cover the configuration where the two limits are mismatched.

### P2 - hygiene / follow-ups, not bench blockers

6. **Test runtimes mix `Builder::new_current_thread()`, `new_multi_thread().worker_threads(N)`, and bare `Runtime::new()`.** Not a production gap, but if RUSSH-3 introduces a shared test harness it should standardize on one builder pattern to avoid masking pool-sizing regressions.
7. **No `thread_name` on per-session runtimes** (#1, #2, #3). Only the daemon hybrid runtime names its threads (`"oc-rsyncd-async"`). RUSSH-3 traces will be easier to attribute if the async-SSH runtimes carry a stable `thread_name` too.
8. **Documentation drift.** Several rustdoc strings under `crates/transfer/` still reference `spawn_blocking` even though the code now uses rayon (see Section 1 "doc-only references"). Out of scope here, but worth flagging for the next comment-cleanup pass.

### Out of scope

- RUSSH-3 bench harness design: this inventory feeds it; harness work happens under #2806.
- Replacing `spawn_blocking` with a dedicated thread pool: tracked separately as a candidate follow-up after RUSSH-3 quantifies the ceiling.
- Migration to async-native transfer pipeline: explicitly off the table per `[[project_no_async_threaded_only]]`.
