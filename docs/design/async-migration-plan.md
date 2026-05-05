# Incremental Async Migration Plan (#1594)

Status: Design (TODO #1594)
Audience: maintainers across `crates/transfer`, `crates/daemon`,
`crates/rsync_io`, `crates/engine`, `crates/bandwidth`, and CI.
Scope: a single roadmap that sequences oc-rsync's move from a
predominantly synchronous concurrency model to a hybrid sync/async
model without breaking the wire protocol, the criterion benchmark
suite, or the upstream interop matrix.

## 1. Summary

oc-rsync is wire-compatible with upstream rsync 3.4.1 (protocol 32).
The transfer hot path is synchronous and rayon-driven; the daemon
listener and the embedded SSH transport are the only places where
async (tokio) leaks into the codebase today. Three merged design
notes specified individual async sub-systems in isolation:

- `docs/design/async-channel-abstraction.md` (#1591) - the
  `TransferChannel` trait and the choice of `flume` for sync/async
  bridges.
- `docs/design/daemon-async-accept-sync-workers.md` (#1674) - the
  hybrid async-accept + sync-worker model for the daemon.
- `docs/design/io-uring-rayon-composition.md` (#1283) - the rule
  that io_uring submissions never block a rayon worker.

This plan sequences those pieces. It defines five phases, the
per-phase exit criteria, and the rollback path at every step. The
migration is wire-compatible at every phase: zero protocol changes,
zero capability flags added, zero new MSG types. The async
migration is a purely internal concurrency model change.

## 2. Status Quo Audit

### 2.1 Already async (tokio-only)

- **Embedded SSH transport (russh-based)**.
  `crates/rsync_io/src/ssh/embedded/connect.rs`,
  `crates/rsync_io/src/ssh/embedded/auth.rs`, and
  `crates/rsync_io/src/ssh/embedded/handler.rs` are async by
  construction because `russh = "0.60.1"` (`Cargo.toml:207`) is a
  tokio-native client. Gated by the `embedded-ssh` feature in
  `crates/rsync_io/Cargo.toml:26-38`.
- **Daemon async listener sketch (#1934 / #1674)**.
  `crates/daemon/src/daemon/async_session/listener.rs`
  (`AsyncDaemonListener`) is the async accept loop. Gated by the
  `async` feature in `crates/daemon/Cargo.toml:16-30`. Not the
  production default. Today's production path is the synchronous
  `serve_connections` in
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`.
- **Bandwidth limiter async wrapper (#1737)**.
  `crates/bandwidth/src/async_limiter.rs` wraps the sync
  `BandwidthLimiter`. Gated by the `async` feature in
  `crates/bandwidth/Cargo.toml:22-31`.
- **Async file-job dispatcher prototypes**.
  `crates/transfer/src/pipeline/async_dispatch.rs` and
  `crates/transfer/src/pipeline/async_pipeline.rs` exist behind
  the `async` feature in `crates/transfer/Cargo.toml:31-32,103`.
  Not wired into the production receiver loop.

### 2.2 Synchronous (the entire transfer hot path)

- **Receiver pipeline**.
  `crates/transfer/src/receiver/transfer/pipeline.rs:31-200`
  (`run_pipeline_loop_decoupled`) is the production receiver entry
  point. Fully blocking: `Read`/`Write` trait bounds, no `.await`.
- **Delta apply**.
  `crates/transfer/src/delta_pipeline.rs:324` sizes the parallel
  delta pipeline by `rayon::current_num_threads()`. The whole
  `delta_apply` family is sync.
- **ReorderBuffer**.
  `crates/transfer/src/reorder_buffer.rs` is a sync ordered queue
  consumed by the disk-commit thread.
- **Network-to-disk SPSC**.
  `crates/transfer/src/pipeline/spsc.rs` is a hand-rolled lock-free
  SPSC over `crossbeam_queue::ArrayQueue`. No syscalls on the hot
  path.
- **Buffer pool**.
  `BufferPool` / `PooledBuffer` in the transfer crate is a
  `Mutex<Vec<Vec<u8>>>` allocator. Sync per #1781.
- **Parallel stat / signature / match**.
  `crates/transfer/src/parallel_io.rs:107-125`,
  `crates/signature/src/parallel.rs:11-86`, and
  `crates/match/src/index/mod.rs:131-217` use rayon directly.
- **Daemon production accept loop**.
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs:216-274`
  is one blocking `accept` followed by `std::thread::spawn` per
  connection.

### 2.3 Workspace runtime invariant

Tokio is the only async runtime. `Cargo.toml:188` declares the
workspace tokio dependency; `Cargo.toml:189` declares `tokio-util`.
The decision to standardise on tokio (and to reject async-std and
smol) was resolved in #1779 and reaffirmed in #1780; this plan does
not reopen it (section 8).

## 3. Why Incremental

Rip-and-replace is not viable. The transfer code, daemon code, test
corpus, criterion benchmarks, and interop golden captures were all
written against the sync model. Flipping every crate to async
simultaneously would:

- Invalidate every integration test in one commit. The golden byte
  tests in `crates/protocol/tests/golden/` exercise wire output
  produced by sync code paths; replacing the producer in one sweep
  yields one colossal unreviewable diff.
- Break the criterion benchmarks (`benchmarks/`, #1285-#1289) for
  the duration of the migration. Regressions hide under noise.
- Force every interop run (`tools/ci/run_interop.sh` against
  upstream 3.0.9, 3.1.3, 3.4.1) to either pass on an untuned path
  or fail in ways hard to bisect.
- Eliminate rollback. The only recovery from a regression would be
  a full revert.

Incremental keeps wire-compat, bench comparability, and single-flip
rollback at every step, and lets the project stop indefinitely at
any phase: phases 4 and 5 are explicitly "if benchmarks justify"
(section 11).

## 4. Five-Phase Roadmap

```
Phase 1  boundary-only async       (today)        [DONE / steady state]
Phase 2  daemon listener flip      (#1674)        [feature: async-daemon]
Phase 3  SSH transport async       (#1890)        [feature: async-ssh, Linux]
Phase 4  receiver pipeline async   (#1079, #1591) [feature: async-transfer]
Phase 5  rayon retreat             (#1283)        [feature: async-default]
```

Each phase is gated behind a feature flag. The flag is default-off
until the per-phase exit criteria in section 5 are met. The flag
remains a runtime kill switch (env var or daemon config) even after
default-on, so any phase can be disabled in production without a
rebuild. See section 6 for rollback.

### 4.1 Phase 1 - boundary-only async (today)

End-of-phase state:

- Tokio spans only the daemon connection-accept layer (when
  `--features async-daemon` is built) and the embedded SSH client.
  Hot transfer code is entirely sync.
- Sync/async bridge is the `TransferChannel` trait from #1591,
  with `flume` as the default. Hot paths keep `crossbeam_channel`
  and the lock-free SPSC.
- `tokio::task::spawn_blocking` is used at exactly one place per
  bridge: the async listener pushes `(TcpStream, SocketAddr)`
  through a `flume::bounded` channel to a sync worker pool.
  `handle_session` is unchanged.

Ships in Phase 1:

- `TransferChannel` / `TransferSender` / `TransferReceiver` traits
  and a flume-backed implementation in
  `crates/transfer/src/channel.rs`, gated by the existing `async`
  feature.
- Guardrail docs ("no second runtime", "no `tokio::sync::Mutex` in
  hot paths") so subsequent phases inherit them.

Does not ship in Phase 1: no production path takes a tokio
dependency unless explicitly opted in; no CLI default mode
requires a runtime. Phase 1 exit gates are met today (section 5.1).

### 4.2 Phase 2 - daemon listener flip

Enable the tokio async accept loop (#1674) behind build-time
`--features async-daemon`. Sync transfer workers unchanged.

Changes:

- `crates/daemon/Cargo.toml`: `async-daemon = ["async"]`,
  default-off.
- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`:
  when `async-daemon` is on, dispatch to
  `crates/daemon/src/daemon/async_session/listener.rs` instead of
  `run_single_listener_loop`. Accept-to-worker hand-off uses the
  Phase 1 flume bridge.
- Sync worker pool sized at `max_connections`, pre-spawned at
  startup. Workers run the existing `handle_session` byte for byte.
- `OC_RSYNC_DAEMON_ASYNC=0` disables the async path at startup
  even on a `--features async-daemon` build.

Safety: the transfer state machine is unchanged; the async layer
touches only bind, accept, socket options, optional reverse DNS,
and the hand-off, none of which mutate wire output. `catch_unwind`
panic isolation is preserved at the sync worker boundary. A
saturated worker pool stalls accept and the kernel SYN backlog
absorbs the burst.

Default-off until benchmarks at 100 / 1k / 10k concurrent
connections show the async accept layer matches or exceeds
`thread::spawn`-per-connection on listings throughput, short-
transfer throughput, and steady-state RSS at 1k idle connections.
Harness lives next to `scripts/benchmark.sh`.

### 4.3 Phase 3 - SSH transport async

Make the SSH transport path async on Linux behind feature
`async-ssh`. The synchronous facade is preserved for non-Linux
callers and any caller that does not want a runtime.

Changes:

- `crates/rsync_io/src/ssh/embedded/` already speaks tokio via
  russh. Phase 3 promotes this to default for SSH transports when
  built with `--features async-ssh` on Linux. macOS, Windows, and
  *BSD keep the system `ssh` subprocess path unless the user opts
  in explicitly. #1890 owns the embedded-ssh validation matrix.
- The sync caller surface (`SshConnection::split()`,
  `SshChildHandle`) is preserved. The runtime is internal to
  `rsync_io`; transfer crates see the same byte pipes.
- The bridge between async socket and sync transfer worker uses
  the Phase 1 `TransferChannel` (`flume::bounded(16)`, per
  `docs/design/async-channel-abstraction.md`). Sync rayon sender
  pushes via `send_blocking`; async forwarder task does
  `recv_async` and `socket.write_all().await`.

Wire bytes are unchanged. The remote-invocation capability string
`-e.LsfxCIvu` is byte-identical to the sync path; `transport::ssh`
changes only how bytes are pushed, not what bytes are pushed.

Default-off until embedded-SSH parity passes against OpenSSH 8.x
and 9.x (#1890), localhost-loop SSH throughput matches sync within
5%, and cipher and KEX stay byte-identical under tcpdump.

Env var rollback: `OC_RSYNC_SSH_ASYNC=0`.

### 4.4 Phase 4 - receiver pipeline async

The first phase that touches the transfer hot path. The delta-apply
pipeline runs in async tasks on the same tokio runtime as the
daemon and SSH transport. ReorderBuffer either becomes a
`tokio::sync::mpsc` / `tokio::sync::oneshot` composite or stays on
crossbeam behind a thin wrapper - benchmark-driven, per
`docs/design/multi-file-delta-apply-pipeline.md` (#1079).

Changes:

- `crates/transfer/src/pipeline/async_pipeline.rs` becomes the
  production receiver pipeline when `async-transfer` is on,
  replacing `run_pipeline_loop_decoupled` in
  `crates/transfer/src/receiver/transfer/pipeline.rs`. Sync
  variant kept as fallback for CLI mode without SSH/daemon.
- Wire-order acks follow #1079: one oneshot per outstanding file,
  producer registers, consumer resolves in send order.
- The disk-commit task is owned by the runtime, not a manually
  spawned `std::thread`. Backpressure uses the Phase 1 bridge.

Risk surface:

- Cancellation. Sync transfers either run to completion or abort
  via `Result::Err`. An async task can be cancelled by a `select!`
  arm dropping its future. We adopt explicit
  `tokio_util::sync::CancellationToken` propagation: the
  `ReceiverContext` owns a token, every spawned task receives a
  clone, and a `Drop` guard in the disk-commit task finalises or
  unlinks the temp file.
- Performance. The lock-free SPSC at
  `crates/transfer/src/pipeline/spsc.rs` is currently 12ns per
  item; any async replacement must match that or we keep crossbeam
  under the bridge.

Default-off until: all wire-format goldens in
`crates/protocol/tests/golden/` green; criterion benchmarks for
delta-apply, multi-file pipeline, and ReorderBuffer within 5% of
sync baseline (#1285-#1289); `tools/ci/run_interop.sh` exits 0
against upstream 3.0.9, 3.1.3, and 3.4.1 with async receiver on.

Env var rollback: `OC_RSYNC_RECEIVER_ASYNC=0`.

### 4.5 Phase 5 - rayon retreat

Finalise the model. Rayon stays for pure-CPU compute (signature
generation, strong-checksum batches); I/O is pure tokio on
async-default builds. The bridge follows
`docs/design/io-uring-rayon-composition.md` (#1283):

- A rayon worker that needs I/O submits non-blocking to the async
  layer, never via a blocking syscall.
- The async layer dispatches CPU-heavy work back to rayon via
  `spawn_blocking` (one-shot, bounded) or `block_in_place` (when
  the worker is already on a runtime thread).
- Rayon pool has a bounded thread count separate from tokio's
  worker pool to avoid mutual starvation, per #1283.

Changes:

- `crates/transfer/src/parallel_io.rs:107-125` and
  `crates/signature/src/parallel.rs:11-86` keep their rayon
  surfaces but expose async wrappers via `spawn_blocking`.
- `crates/match/src/index/mod.rs` candidate verification stays
  rayon-internal; the caller is async-blind.
- Daemon, SSH transport, and receiver share one tokio runtime.

Default-off until: Phase 4 has been default-on for at least one
minor release without field regressions; rayon pool sizing tuned
against tokio worker count on the criterion suite (CI exercises
1, 4, 8, 16, 32 logical CPUs); a runbook for tuning the two pools
ships with the Phase 5 PR.

Env var rollback: `OC_RSYNC_ASYNC=0` is the master kill switch
across daemon, SSH, and receiver paths.

## 5. Per-Phase Exit Criteria

Each phase has the same four exit gates. The phase does not flip
default-on until all four are green simultaneously.

1. **Wire compatibility.** The golden byte tests in
   `crates/protocol/tests/golden/` stay green. A `tcpdump`
   capture against upstream rsync 3.4.1 replays byte-identical
   for the new async path on the matrix of:
   - protocol negotiation (versions 28-32),
   - file-list transfer (with and without INC_RECURSE),
   - delta-apply for at least one binary file and one text file,
   - itemize output for create / update / delete cases.
2. **Performance.** No regression on the criterion suite under
   `benchmarks/` (the suite added in #1285-#1289). The threshold
   is +5% on any individual benchmark and +2% on the geometric
   mean across all benchmarks. CI fails the phase rollout if
   either threshold is exceeded.
3. **Coverage.** No drop below 95% line coverage measured by
   `cargo llvm-cov`. The current line coverage targets are
   tracked in #1107 and #1774; the phase rollout PR includes a
   coverage report as a CI artefact.
4. **Interop.** `tools/ci/run_interop.sh` exits 0 against
   upstream rsync 3.0.9, 3.1.3, and 3.4.1, in both client and
   server roles, in both push and pull directions, against the
   daemon with each phase's feature flag enabled.

### 5.1 Phase 1 status (already met)

- Wire-compat: phase 1 ships no production code path that
  produces wire bytes; the boundary async only touches accept
  and the SSH socket. All goldens green at HEAD.
- Performance: no production hot path changed. Bench numbers
  identical to the sync baseline.
- Coverage: above 95% at HEAD.
- Interop: green on all three upstream versions.

Phase 1 is the steady state today.

## 6. Rollback at Each Phase

Every phase is feature-gated at build time and kill-switched at
runtime:

| Phase | Build feature      | Runtime kill switch                   |
|-------|--------------------|---------------------------------------|
| 1     | n/a (always on)    | n/a (no production hot path uses it)  |
| 2     | `async-daemon`     | `OC_RSYNC_DAEMON_ASYNC=0` env or daemon config `async = false` |
| 3     | `async-ssh`        | `OC_RSYNC_SSH_ASYNC=0` env            |
| 4     | `async-transfer`   | `OC_RSYNC_RECEIVER_ASYNC=0` env       |
| 5     | `async-default`    | `OC_RSYNC_ASYNC=0` (master kill)      |

A user who hits a regression in production sets the env var (or
edits `oc-rsyncd.conf` for the daemon) and the binary falls back to
the sync path on the next process start. No rebuild is required.
The runtime kill switch is checked once at startup, not per
request, so the cost is zero on the hot path.

The build-time feature lets distributors ship oc-rsync with async
disabled entirely if they want to minimise the dependency surface
(no tokio in the binary). The default Cargo build for end users
includes the async features once each phase has flipped default-on.

## 7. Wire-Compat Invariant

**Zero protocol changes across all five phases.** The async
migration is a purely internal concurrency model change. To make
this concrete:

- No new MSG_ frame types in `crates/protocol/src/messages/`.
- No new capability flags in
  `crates/transfer/src/setup.rs::build_capability_string` (the
  current value is `-e.LsfxCIvu` and stays that way).
- No new daemon `@RSYNCD:` greeting variants.
- No new exit codes in `crates/core/src/exit_code.rs`.
- The varint and 4-byte LE legacy framing in `crates/protocol/src`
  is unchanged.
- The `tcpdump` replay test ships with each phase rollout PR and
  must produce byte-identical output to the sync path.

A change that requires a wire-format extension is by definition
out of scope for this plan and must be proposed in a separate
design note. The async migration is not a vehicle for protocol
extensions; the project rule is that wire-format work is rare and
explicit (see the user-feedback rule on no wire protocol features
for niche performance gains).

## 8. Risk Register

### 8.1 Dual-runtime overhead and CLI startup latency

Tokio at startup costs a thread pool, a reactor, and a timer
wheel - roughly 100-300 microseconds and a few MB of RSS even on
an idle process. CLI mode for local-only transfers
(`oc-rsync src/ dst/`) does not need a runtime.

Mitigation: lazy runtime construction. The runtime is built only
if the CLI invokes the SSH transport, the daemon, or the
`rsync://` scheme. Local-only transfers stay sync end-to-end. CLI
uses a single current-thread runtime; multi-thread is reserved for
the daemon. The runtime is built once and reused for the process
lifetime. CI fails if `oc-rsync --version` startup regresses by
more than 10ms.

### 8.2 Async cancellation semantics differ from sync abort

A sync transfer aborts via `Result::Err` propagation. An async
task can be cancelled by its parent dropping the future, leaving
no opportunity for cleanup unless the task holds a `Drop` guard
or explicitly handles cancellation.

Mitigation: explicit `tokio_util::sync::CancellationToken`
propagation; the `ReceiverContext` owns a token, every spawned
task receives a child token. Half-applied state lives behind RAII
guards: the disk-commit task's `Drop` impl finalises or unlinks
the temp file. Phase 4 exit criteria include a kill-test - the
receiver is cancelled at random points in 1000 transfers and
every result must leave a consistent destination tree.

### 8.3 Rayon pool starvation under `block_in_place`

`block_in_place` lets a tokio worker run sync work while the
runtime steals other tasks. Two failure modes: the rayon pool
fans back out to tokio via channels and the parked worker
deadlocks the pool below its minimum; or rayon's global pool and
tokio's workers fight over the same CPUs.

Mitigation: rayon pool has a bounded thread count separate from
tokio's worker pool, per #1283. `block_in_place` is reserved for
one-shot CPU-heavy compute (signature, strong-checksum batch); it
is never used to bridge channels. The cross-pool bridge is
`spawn_blocking`, which uses tokio's separate blocking-pool.

### 8.4 Bridge crate stability

The migration depends on `flume` (Phase 1) and
`tokio_util::sync::CancellationToken` (Phase 4). Both pinned in
the workspace `Cargo.toml` (`flume = "=0.11.x"`,
`tokio-util = "0.7"` at `Cargo.toml:189`). Phase rollout PRs
require `cargo audit` clean.

## 9. Pinned External Decisions

The following are settled and not re-litigated by this plan.

- **No second async runtime.** Tokio is the only async runtime
  in the workspace (`Cargo.toml:188`). #1779 verified this and
  #1780 confirmed scope. We do not introduce `async-std`,
  `smol`, `monoio`, `glommio`, or `embassy`. A future need for
  one of these would be a separate design note with explicit
  re-evaluation of the trade-offs.
- **No `tokio::sync::Mutex` in hot paths.** Per #1781, the hot
  paths use `std::sync::Arc<Mutex<T>>`. `tokio::sync::Mutex` is
  permitted only in async-only state (e.g. the listener's
  `max_connections` semaphore in
  `crates/daemon/src/daemon/async_session/listener.rs`). The
  `BufferPool` stays on `std::sync::Mutex<Vec<Vec<u8>>>` even
  when the receiver pipeline is async (Phase 4).
- **`flume` for sync/async bridges.** Per
  `docs/design/async-channel-abstraction.md` (#1591), the
  bridge crate is `flume` with the `TransferChannel` trait
  abstraction. `crossbeam_channel` stays on the pure-sync hot
  paths. `tokio::sync::mpsc` is permitted only inside fully
  async sub-graphs (e.g. the listener's internal control plane).

## 10. Test Strategy Per Phase

Each phase ships with new integration tests; existing tests are
not modified (so the existing sync path stays under continuous
test coverage even after the async path becomes default-on).

- **Phase 1**. Unit tests for the `TransferChannel` trait and the
  flume implementation. Round-trip tests for sync producer / async
  consumer and the reverse direction. Already shipped under #1591.
- **Phase 2**. Daemon-level integration tests that drive the
  async accept layer and assert byte-for-byte parity with the
  sync accept layer for module listing, auth handshake, and a
  small file transfer. Concurrency tests at 100 / 1k / 10k
  connections.
- **Phase 3**. Embedded-SSH parity tests against OpenSSH 8.x and
  9.x. Cipher and KEX byte-equivalence under tcpdump. Throughput
  benchmarks on localhost loop. (#1890 specifies the matrix.)
- **Phase 4**. Receiver async pipeline integration tests
  exercising the full delta-apply path. Cancellation kill-tests
  (section 8.3). Multi-file pipeline ordering tests per #1079.
- **Phase 5**. Cross-pool stress tests (signature generation
  under high tokio I/O load). Rayon-tokio interaction tests
  validating no starvation under `block_in_place`.

The criterion suite (`benchmarks/`) is run on every PR that
flips a phase default-on and the per-phase performance gate
(section 5) blocks the merge if it regresses.

## 11. Migration Sequencing

The dependency graph is strict:

```
Phase 1
  |
  +--> Phase 2 (needs the flume bridge)
  |       |
  |       +--> Phase 3 (needs Phase 2's daemon flip stable)
  |               |
  |               +--> Phase 4 (needs Phases 2 and 3 stable)
  |                       |
  |                       +--> Phase 5 (after Phase 4 default-on for
  |                                     one minor release)
```

- Phase 2 cannot ship without Phase 1's `TransferChannel`. The
  bridge crate is a build-time prerequisite.
- Phase 3 needs Phase 2 default-on long enough to verify the
  daemon does not regress; embedded SSH shares the same tokio
  runtime, so any pool-starvation bug must surface on the
  simpler daemon path first.
- Phase 4 needs both 2 and 3 stable, because the receiver runs
  inside the same runtime as the daemon listener and the SSH
  transport.
- Phase 5 cannot start until Phase 4 has been default-on for one
  minor release with clean field reports.

Phase 4 needing both 2 and 3 is the only branch; otherwise the
order is linear.

## 12. Exit at Any Phase

The plan supports stopping indefinitely at the end of any phase.

- Phase 1 is the current steady state. We can stay here forever
  if Phase 2's benchmarks do not justify the daemon flip.
- Phase 2 alone is operationally complete: it solves the 1k-10k
  concurrent-connection use case (#1674) without touching the
  transfer hot path.
- Phase 3 alone is operationally complete for users who care
  about embedded-SSH performance on Linux.
- Phase 4 is explicitly "if benchmarks justify". If the sync
  receiver continues to meet the performance targets, the
  cancellation-semantics complexity is not worth it.
- Phase 5 is final consolidation - desirable for code
  cleanliness but not for correctness or performance.

The plan is not a one-way door. The sync path remains supported
at every phase.

## 13. Tracking

This plan sequences existing pending TODOs. It does not introduce
new ones.

- **#1590** - tokio vs async-std. Resolved to tokio per #1779;
  this plan codifies that (section 9) and closes #1590 as
  superseded.
- **#1591** - async-compatible channel abstraction. Phase 1.
  Already shipped; `docs/design/async-channel-abstraction.md`.
- **#1593** - async I/O for SSH transport. Phase 3.
  Implementation vehicle is `crates/rsync_io/src/ssh/embedded/`;
  bridge is the Phase 1 `TransferChannel`.
- **#1594** - this plan.
- **#1595** - async impact on io_uring. Folded into Phase 5;
  the io_uring + rayon composition rule is already specified at
  `docs/design/io-uring-rayon-composition.md`.
- **#1674** - daemon async accept + sync workers. Phase 2.
  Design at `docs/design/daemon-async-accept-sync-workers.md`.
- **#1779**, **#1780**, **#1818** - sole-runtime audits. Closed;
  cited in section 9.
- **#1781** - hot-path mutex policy. Cited in section 9.
- **#1782-#1814** - embedded SSH series. Phase 3 enabling work.
- **#1890** - embedded SSH validation matrix. Phase 3 gate.
- **#1934** - async daemon listener implementation. Phase 2.
- **#1283 / #1284** - io_uring + rayon composition. Phase 5
  prerequisite (rule already exists; Phase 5 adopts it).
- **#1285-#1289** - criterion benchmark suite that gates each
  phase rollout.
- **#1107**, **#1774** - line coverage targets that gate each
  phase rollout.
- **#1079** - multi-file delta-apply pipeline; supplies the
  wire-order ack pattern Phase 4 reuses.
- **#1737** - bandwidth limiter async wrapper. Phase 1
  prerequisite, already shipped.

## 14. References

- `docs/design/async-channel-abstraction.md` (#1591).
- `docs/design/daemon-async-accept-sync-workers.md` (#1674).
- `docs/design/io-uring-rayon-composition.md` (#1283).
- `docs/design/multi-file-delta-apply-pipeline.md` (#1079).
- `Cargo.toml:188-200` - workspace tokio, tokio-util, rayon,
  crossbeam, dashmap pins.
- `crates/daemon/Cargo.toml:16-30` - daemon `async` feature.
- `crates/transfer/Cargo.toml:31-32,103` - transfer `async`
  feature.
- `crates/bandwidth/Cargo.toml:22-31` - bandwidth `async`
  feature.
- `crates/rsync_io/Cargo.toml:26-38` - rsync_io `embedded-ssh`
  feature.
- `crates/daemon/src/daemon/async_session/listener.rs` - async
  accept implementation.
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:106,216-281`
  - sync accept loop (today's production path).
- `crates/transfer/src/receiver/transfer/pipeline.rs:31-200` -
  sync receiver pipeline.
- `crates/transfer/src/pipeline/spsc.rs` - lock-free SPSC, stays
  sync.
- `crates/transfer/src/pipeline/async_pipeline.rs`,
  `crates/transfer/src/pipeline/async_dispatch.rs` - async
  prototypes Phase 4 promotes.
- `crates/transfer/src/reorder_buffer.rs` - sync ordered queue
  reconsidered in Phase 4.
- `crates/transfer/src/parallel_io.rs:107-125`,
  `crates/signature/src/parallel.rs:11-86`,
  `crates/match/src/index/mod.rs:131-217` - rayon CPU-bound
  paths that stay rayon in Phase 5.
