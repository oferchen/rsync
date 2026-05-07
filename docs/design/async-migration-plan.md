# Async Migration Plan for the Transfer Pipeline (#1594)

Status: Design proposal for #1594, supersedes the prior 5-phase sketch.
Audience: maintainers across `crates/transfer`, `crates/daemon`,
`crates/rsync_io`, `crates/engine`, `crates/bandwidth`,
`crates/protocol`, and `crates/core`; CI owners; release engineers.
Scope: a single roadmap that sequences oc-rsync's hybrid sync/async
model from today's seven-crate tokio surface to a stable end-state,
without breaking the wire protocol, the criterion benchmark suite,
or the upstream interop matrix.

This revision aligns the migration narrative with
`docs/audits/tokio-dependency-boundary-2026.md` (PR #3706, the
re-verification of the #1779 boundary) and
`docs/audits/daemon-thread-per-connection-scalability.md`
(PR #3705, the daemon scalability ceiling Phase 2 addresses).
Together they replace the rougher framing the earlier roadmap
inherited from #1779.

## 1. Motivation

### 1.1 What is broken today

The synchronous accept loop
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:106-141`)
spawns one OS thread per connection, with a hard ceiling near
`max_connections * (8MB stack + per-thread state)`. Idle daemon
RSS grows linearly in concurrent connections; the scalability
audit (`docs/audits/daemon-thread-per-connection-scalability.md`)
documents the headroom we lose at 1k-10k idle connections.

The CLI client side sees a related cost: the SSH transport
(`crates/rsync_io/src/ssh/connection.rs:30-178`) exposes a sync
`Read`/`Write` facade, but its embedded backend
(`crates/rsync_io/src/ssh/embedded/connect.rs:107-122`) already
builds a tokio current-thread runtime and bridges sync I/O over a
russh channel. The runtime is paid for; only the surface stays
sync.

### 1.2 What we lose if we do nothing

- The daemon stays bound by thread-per-connection at exactly the
  workloads (high-fanout backups, archive distribution) where it
  is most useful as a long-running service.
- Async-capable callers cannot drive oc-rsync without spinning
  their own threads, because the public APIs do not expose async
  surfaces beyond the seven feature-gated crates.
- Each new feature that wants async ergonomics has to invent its
  own bridge, which is how we drifted from "tokio in 2 crates" to
  "tokio in 7 crates" without an explicit design pass (per the
  audit at `docs/audits/tokio-dependency-boundary-2026.md`).

### 1.3 Why a plan exists at all

The seven-crate tokio surface has already been reached. The
question this plan answers is not "should we adopt tokio" but
"how do we sequence the remaining work and keep wire-compat,
benchmark comparability, and rollback at every step."

## 2. Current State Inventory

### 2.1 Async surfaces that exist today

Every `pub async fn` and `pub fn -> impl Future` in the workspace
is feature-gated. Citations are file:LINE in the current tree.

- `bandwidth`: `AsyncRateLimiter::consume`
  (`crates/bandwidth/src/async_limiter.rs:77`),
  `AsyncRateLimiter::reset` (`async_limiter.rs:107`).
- `transfer`: `produce_file_jobs`
  (`crates/transfer/src/pipeline/async_dispatch.rs:29`),
  `run_pipeline` returning `impl Future`
  (`crates/transfer/src/pipeline/async_pipeline.rs:137`).
- `protocol`: `NegotiationPrologueSniffer::read_from_async`
  (`crates/protocol/src/negotiation/sniffer/async_read.rs:53`).
- `daemon`: `AsyncSession::handle`
  (`crates/daemon/src/daemon/async_session/session.rs:68`),
  `AsyncSession::acquire` (`session.rs:247`),
  `AsyncDaemonListener::bind`
  (`crates/daemon/src/daemon/async_session/listener.rs:128`),
  `AsyncDaemonListener::serve` (`listener.rs:180`),
  `AsyncDaemonListener::accept_one` (`listener.rs:264`).
- `engine`: `AsyncFileCopier::copy_file`
  (`crates/engine/src/async_io/copier.rs:91`),
  `AsyncFileCopier::copy_file_with_progress` (`copier.rs:108`),
  `AsyncBatchCopier::copy_files`
  (`crates/engine/src/async_io/batch.rs:113`).
- `rsync_io`: `resolve_host`
  (`crates/rsync_io/src/ssh/embedded/resolve.rs:26`),
  `authenticate` (`auth.rs:233`).

The seven crates that own these surfaces - `bandwidth`, `core`,
`daemon`, `engine`, `protocol`, `rsync_io`, `transfer` - are
exactly the allowed set per the audit. Items outside this set
remain forbidden.

### 2.2 Workspace runtime knobs

- `Cargo.toml:107` defines the bin-level `async = ["daemon/async",
  "core/async"]` umbrella feature; it sits in the default feature
  list at `Cargo.toml:24-35`.
- `Cargo.toml:188` pins the workspace tokio version with features
  `rt-multi-thread, io-util, net, sync, time, macros`.
- `Cargo.toml:189` pins `tokio-util` at version 0.7 with `codec`
  and `io` features.
- Each per-crate gate threads `dep:tokio`:
  `crates/bandwidth/Cargo.toml:22,27`,
  `crates/core/Cargo.toml:44,90,93`,
  `crates/daemon/Cargo.toml:20,45`,
  `crates/engine/Cargo.toml:37,97`,
  `crates/protocol/Cargo.toml:29,49-50`,
  `crates/rsync_io/Cargo.toml:26,32`,
  `crates/transfer/Cargo.toml:31-32,104`.

### 2.3 Sync hot-path surfaces (intentionally not async)

- Production daemon accept loop: `serve_connections`
  (`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`),
  `spawn_connection_worker`
  (`.../server_runtime/connection.rs:106-141`),
  `run_single_listener_loop` (`connection.rs:216`).
- Sync receiver pipeline: `run_pipeline_loop_decoupled`
  (`crates/transfer/src/receiver/transfer/pipeline.rs:38`).
- Lock-free SPSC: `Sender`/`Receiver`
  (`crates/transfer/src/pipeline/spsc.rs:68,103`).
- Bounded reorder buffer: `BoundedReorderBuffer`
  (`crates/transfer/src/reorder_buffer.rs:57,79`).
- Rayon CPU paths: `parallel_io.rs:124`,
  `crates/signature/src/parallel.rs:84,207`,
  `crates/match/src/index/mod.rs:135,208`.
- Sync bandwidth limiter: `BandwidthLimiter`
  (`crates/bandwidth/src/limiter/core/limiter.rs:34`).
- Sync SSH facade: `SshConnection`
  (`crates/rsync_io/src/ssh/connection.rs:30,178`).
- Buffer pool: `BufferPool::acquire`/`try_acquire`
  (`crates/engine/src/local_copy/buffer_pool/pool.rs:459,488`).

### 2.4 The boundary the audit pins

`docs/audits/tokio-dependency-boundary-2026.md` recommends a CI
guardrail `tools/ci/check_tokio_boundary.sh` that enforces:
tokio appears only in the seven crates listed in 2.1, every direct
dep is `optional = true`, and no other workspace crate names tokio
in `[dependencies]` or `[target.cfg(...)]`. This plan treats that
guardrail as a Phase 0 prerequisite (section 4.1).

## 3. Target End-State

### 3.1 What goes async

- Daemon accept layer: socket bind, accept, per-connection handoff,
  reverse DNS, proxy-protocol parsing. Per
  `docs/design/daemon-async-accept-sync-workers.md` (#1674).
- SSH transport bytestream: connect, key exchange, authentication,
  per-channel send/recv. Already true on the `embedded-ssh` path;
  Phase 3 promotes it to the default for that feature.
- Transfer pipeline orchestration: file-job production, retry
  scheduling, cancellation, progress aggregation. The
  `run_pipeline` future at
  `crates/transfer/src/pipeline/async_pipeline.rs:137` is the
  vehicle; Phase 4 wires it into the production receiver.
- Rate limiting: token-bucket sleep on the I/O path. Already
  implemented at `crates/bandwidth/src/async_limiter.rs:30-77`;
  Phase 1 ratifies the public surface.
- Multiplex codec: framing on the wire side. `MultiplexCodec`
  exists in `crates/protocol/src/multiplex/`; Phase 3 makes it the
  default for async-built transports.

### 3.2 What stays synchronous

- All CPU-bound compute. Rolling and strong checksums, delta
  matching, signature generation, compression frame encoding,
  filter rule evaluation. These run on rayon's pool.
- Platform fast-path I/O. The `fast_io` crate stays tokio-free;
  io_uring (`#[cfg(all(target_os = "linux", feature = "io_uring"))]`),
  IOCP, `copy_file_range`, `clonefile`, `CopyFileExW`. Async layers
  call into `fast_io` via `spawn_blocking` when they need an
  fd-bound operation.
- Lock-free SPSC at
  `crates/transfer/src/pipeline/spsc.rs:68,103`. The wakeup
  cost of `tokio::sync::mpsc` exceeds the spin-wait cost we
  measure, and the SPSC is one consumer one producer by
  construction.
- Buffer pool and other contention-sensitive shared state. Per the
  hot-path mutex policy (no `tokio::sync::Mutex` in hot paths).

### 3.3 The dividing line

Async owns scheduling (when work runs, who runs it next, when to
cancel). Synchronous owns computation (CPU-heavy, fd-bound, or
SPSC-paced steps). The bridge is `spawn_blocking` for one-shot
CPU jumps, `block_in_place` for in-task CPU jumps, and the
`TransferChannel` trait of #1591 for sustained traffic in either
direction.

## 4. Migration Phases

```
Phase 0  boundary guardrail        [tools/ci/check_tokio_boundary.sh]
Phase 1  ratify the seven crates   [no code change, policy update]
Phase 2  daemon listener default   [feature: async-daemon, env kill switch]
Phase 3  SSH transport default     [feature: async-ssh, Linux first]
Phase 4  receiver pipeline default [feature: async-transfer]
Phase 5  rayon-tokio composition   [feature: async-default, master kill switch]
```

Each phase ships behind a build-time feature and a runtime kill
switch. The flag stays default-off until the four exit gates of
section 7 are all green.

### 4.1 Phase 0 - boundary guardrail

Scope: add `tools/ci/check_tokio_boundary.sh` (30-50 lines of
bash). Wire it into the existing CI lint job alongside
`tools/no_placeholders.sh` and `tools/enforce_limits.sh`. No code
change in `crates/`.

Entry point: the audit at
`docs/audits/tokio-dependency-boundary-2026.md:352-368` already
specifies the script's contract. The script reads each
`crates/*/Cargo.toml`, scopes to `[dependencies]` and
`[target.'cfg(...)'.dependencies]`, greps for `^tokio` or
`^tokio-util`, and compares the resulting crate set against the
seven-crate allow-list.

Exit criteria: the script exits zero against the current tree,
exits nonzero on a synthetic violation (a test fixture that adds
tokio to `crates/cli/Cargo.toml`), and runs in under 5 seconds on
the existing CI runners.

Blast radius: zero code, one new tool. The only failure mode is a
PR that drifts the boundary, which is exactly the failure the
script catches.

### 4.2 Phase 1 - ratify the seven crates

Scope: codify in this document and in workspace policy notes that
the seven-crate boundary is the steady state. No code change.

Entry point: the audit's "Recommendation" section at
`docs/audits/tokio-dependency-boundary-2026.md:316-348` enumerates
the corrected policy text. Phase 1 lands that text and removes the
stale "tokio: only daemon and core" framing from #1779.

Exit criteria: every `docs/` reference to "tokio in daemon and
core" updated; unsafe-code policy stays disjoint from the tokio
policy. Blast radius: documentation only.

### 4.3 Phase 2 - daemon listener default

Scope: flip the production daemon path from sync accept
(`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`)
to the async listener
(`crates/daemon/src/daemon/async_session/listener.rs:128-264`)
when built with `--features async-daemon`. Sync transfer workers
unchanged; the bridge follows
`docs/design/daemon-async-accept-sync-workers.md` (#1674).

Entry point: `AsyncDaemonListener::serve` already spawns a
per-connection async handler at `listener.rs:216`. Phase 2 adds a
build-time selector in `accept_loop.rs` that picks between
`run_single_listener_loop` (sync) and `AsyncDaemonListener::serve`
(async) based on the `async-daemon` feature.

Exit criteria: section 7 gates green; concurrency tests at 100,
1k, and 10k idle connections show async accept matches or exceeds
sync on listings throughput, short-transfer throughput, and
steady-state RSS; `OC_RSYNC_DAEMON_ASYNC=0` reverts to sync on the
next process start without a rebuild.

Blast radius: the daemon binary only. CLI client paths unchanged.
The transfer state machine is unchanged; the async layer touches
bind, accept, socket options, optional reverse DNS, and the
sync-worker handoff, none of which mutate wire output.

### 4.4 Phase 3 - SSH transport default on Linux

Scope: when built with `--features async-ssh` on Linux, route SSH
through the embedded backend at
`crates/rsync_io/src/ssh/embedded/connect.rs:107-122` (which
already builds a tokio current-thread runtime) instead of exec-ing
the system `ssh` binary. macOS, Windows, and BSDs keep the
subprocess path unless explicitly opted in.

Entry point: `connect_and_exec` is the sync facade
(`connect.rs:107`); `connect_and_exec_async` (`connect.rs:125`) is
the async core. Phase 3 lifts the async core to a top-level
public surface for callers that already own a runtime, keeps the
sync facade for callers that do not.

Exit criteria: embedded-SSH parity per #1890 (OpenSSH 8.x and 9.x,
byte-identical KEX and cipher under tcpdump); localhost throughput
within 5% of system-ssh; `OC_RSYNC_SSH_ASYNC=0` reverts to
subprocess.

Blast radius: SSH transport only. Wire bytes unchanged; the
capability string `-e.LsfxCIvu` is byte-identical on both paths.

### 4.5 Phase 4 - receiver pipeline default

Scope: replace the sync receiver pipeline at
`crates/transfer/src/receiver/transfer/pipeline.rs:38`
(`run_pipeline_loop_decoupled`) with the async pipeline at
`crates/transfer/src/pipeline/async_pipeline.rs:137` (`run_pipeline`)
when built with `--features async-transfer`. The sync variant
stays as the fallback for builds without `async`.

Entry point: `run_pipeline` already accepts a closure mapping
`FileJob` to a future. Phase 4 wires its caller (the receiver
context site) to provide that closure. The `PipelineHandle`
carries a `tokio_util::sync::CancellationToken`; cancellation
matches #1079's wire-order ack pattern.

Exit criteria: section 7 gates green; cancellation kill-test
(receiver cancelled at random points in 1000 transfers leaves a
consistent tree, no half-applied temp files); #1079 ordering
tests green; `OC_RSYNC_RECEIVER_ASYNC=0` reverts on next call.

Blast radius: the entire transfer hot path. Highest-risk phase;
mitigations in section 9.

### 4.6 Phase 5 - rayon-tokio composition

Scope: finalise the bridge rules from
`docs/design/io-uring-rayon-composition.md` (#1283). Rayon owns
CPU compute (signature generation, candidate verification at
`crates/match/src/index/mod.rs:135,208`); tokio owns I/O
scheduling. The pools are sized independently, do not steal from
each other, and exchange work only through `spawn_blocking` or
`TransferChannel` (#1591).

Entry point: `crates/transfer/src/parallel_io.rs:124` and
`crates/signature/src/parallel.rs:84,207` keep their rayon
surfaces; new async wrappers sit beside them gated by
`async-default`. See `docs/design/adaptive-thread-pool-sizing.md`
for #1751's worker-count heuristics.

Exit criteria: Phase 4 default-on for one minor release without
field regressions; cross-pool stress tests show no rayon
starvation under tokio I/O load; a tuning runbook ships with the
PR; `OC_RSYNC_ASYNC=0` master kill switch covers all paths.

Blast radius: workspace-wide. Final consolidation; not a
correctness or wire-compat change.

## 5. The Sync/Async Bridge Problem

### 5.1 Five bridge primitives, one rule per direction

- **Sustained sync to async**: `flume::bounded(N)` via
  `TransferChannel` (#1591). Network-to-disk, signature-to-pipeline.
- **Sustained async to sync**: same, `recv_async` on the async
  side. Job dispatch to rayon.
- **One-shot async to sync**: `tokio::task::spawn_blocking`. Per-file
  CPU compute.
- **One-shot sync to async**: `Handle::block_on`, only in CLI
  startup, driving a single async call from `main()`.
- **In-task sync burst**: `tokio::task::block_in_place`. Short
  rayon op inside an async task.

The rule is: pick the primitive on the call's lifetime, not on the
calling crate. A signature pass per file is `spawn_blocking`; a
per-byte token-bucket sleep is `await` on the async limiter; a
sustained network-to-disk feed is `TransferChannel`.

### 5.2 Where #1751 fits

#1751 governs the one-shot bridge: when an async task needs a
rayon-sized burst, the bridge is `spawn_blocking` returning a
`JoinHandle<T>`. The blocking pool is separate from the worker
pool, so the worker is never parked on rayon. The opposite
direction (a rayon worker pushing to the async layer) goes through
the `TransferChannel` trait at the caller of
`crates/transfer/src/pipeline/async_dispatch.rs:29`, not through
`Handle::block_on`. Calling `block_on` from a rayon worker is a
deadlock if the worker holds any runtime resource.

### 5.3 What we do not bridge

`crates/transfer/src/pipeline/spsc.rs:68,103` stays sync at both
ends; wrapping it in async would force a wakeup-per-item cost
~10x the spin-wait cost. If either side ever moves to tokio, the
SPSC consumer becomes a `block_in_place` boundary on the async
side, never a `tokio::sync::mpsc` replacement.

## 6. Backwards-Compatibility Constraints

### 6.1 Wire protocol

Zero changes across all phases. Concretely:

- No new `MSG_*` frame types in `crates/protocol/src/messages/`.
- No new capability-string flags. The current value
  `-e.LsfxCIvu` (built in `crates/transfer/src/setup.rs::build_capability_string`)
  stays exactly that.
- No new daemon `@RSYNCD:` greeting variants.
- No new exit codes in `crates/core/src/exit_code.rs`.
- The varint and 4-byte LE legacy framing in `crates/protocol/src`
  is unchanged.

A `tcpdump` replay test ships with each phase rollout PR and must
produce byte-identical output to the sync path. Wire-level
extensions are explicitly out of scope; this plan is a concurrency
refactor, not a protocol vehicle.

### 6.2 CLI flags

- No new flags introduced by this plan. The async path is opt-in
  via build features and opt-out via env vars; users who do not
  want a runtime build with `--no-default-features`.
- `--no-default-features` builds remain tokio-free per the audit's
  feature graph. Distributors who want a minimal binary keep that
  path.
- All existing flags retain their meaning. The async receiver
  honours `--bwlimit`, `--timeout`, `--contimeout`, and
  `--partial-dir` byte-for-byte against the sync receiver.

### 6.3 Public API

- The seven async-bearing crates already publish their
  feature-gated async items. Phase 1 ratifies the surface; later
  phases do not add new public types beyond what the existing
  `crates/*/src/lib.rs` files already export under
  `#[cfg(feature = "async")]`.
- The 18 crates outside the seven-crate allow-list stay tokio-free
  (full list in `docs/audits/tokio-dependency-boundary-2026.md:181-185`).
  Phase 0's CI guardrail enforces this.

## 7. Test Strategy Per Phase

Every phase ships with new tests; existing tests are not modified
so the sync path stays under continuous coverage even after the
async path becomes default-on.

### 7.1 Per-phase exit gates (applied to phases 2-5)

Each phase has the same four gates and does not flip default-on
until all four are green simultaneously.

1. **Wire compatibility.** Golden byte tests in
   `crates/protocol/tests/golden/` stay green. A `tcpdump`
   capture against upstream rsync 3.4.1 replays byte-identical
   for the async path on the matrix of: protocol negotiation
   (versions 28-32), file-list transfer (with and without
   INC_RECURSE), delta-apply for at least one binary file and
   one text file, itemize output for create / update / delete.
2. **Performance.** No regression on the criterion suite under
   `benchmarks/`. Threshold: +5% on any individual benchmark and
   +2% on the geometric mean. CI fails the rollout if either
   threshold is exceeded.
3. **Coverage.** No drop below 95% line coverage measured by
   `cargo llvm-cov`. The phase rollout PR includes a coverage
   report as a CI artefact.
4. **Interop.** `tools/ci/run_interop.sh` exits 0 against
   upstream rsync 3.0.9, 3.1.3, and 3.4.1 in both client and
   server roles, in both push and pull directions, with the
   feature flag enabled.

### 7.2 Phase-specific tests

- **Phase 0**: fixture `tools/ci/test_check_tokio_boundary.sh`
  feeds a synthetic violation (tokio in a forbidden crate),
  asserts nonzero; clean tree asserts zero.
- **Phase 1**: lint check on `docs/` for stale "tokio in daemon
  and core" wording.
- **Phase 2**: daemon integration tests asserting byte parity
  with sync accept for module listing, auth handshake, small
  file transfer. Concurrency tests at 100, 1k, 10k connections.
- **Phase 3**: embedded-SSH parity against OpenSSH 8.x and 9.x
  per #1890; KEX and cipher byte-equivalence under tcpdump.
- **Phase 4**: full receiver async integration over delta-apply;
  cancellation kill-tests (section 9.2); multi-file ordering
  per #1079.
- **Phase 5**: cross-pool stress; signature under tokio I/O load;
  no starvation under `block_in_place`.

The criterion suite runs on every default-flipping PR; the
per-phase performance gate blocks the merge.

## 8. Rollout and Feature Flag Plan

### 8.1 Build-time features

| Phase | Build feature | Default | Pulls |
|-------|---------------|---------|-------|
| 0 | n/a | always | tools/ci script only |
| 1 | n/a | always | docs only |
| 2 | `async-daemon` | off until exit gates | `daemon/async`, `core/async` |
| 3 | `async-ssh` | off until exit gates | `rsync_io/embedded-ssh` plus `core/async` |
| 4 | `async-transfer` | off until exit gates | `transfer/async`, `engine/async` |
| 5 | `async-default` | off until exit gates | umbrella; all of the above |

The bin-level umbrella `async = ["daemon/async", "core/async"]` at
`Cargo.toml:107` already exists and stays default-on. Phase
features compose on top; e.g. `async-daemon` enables
`AsyncDaemonListener` as the production accept loop, not just as
a feature-gated type.

### 8.2 Runtime kill switches

| Phase | Env var | Effect |
|-------|---------|--------|
| 2 | `OC_RSYNC_DAEMON_ASYNC=0` | Daemon falls back to thread-per-connection |
| 3 | `OC_RSYNC_SSH_ASYNC=0` | SSH transport falls back to subprocess |
| 4 | `OC_RSYNC_RECEIVER_ASYNC=0` | Receiver falls back to sync pipeline |
| 5 | `OC_RSYNC_ASYNC=0` | Master kill switch across all paths |

The kill switch is checked once at startup, not per request, so
it costs zero on the hot path. A user who hits a regression sets
the env var (or edits `oc-rsyncd.conf` for the daemon) and the
binary falls back on the next process start; no rebuild needed.

### 8.3 Distributor builds

`--no-default-features` plus the non-async perf and metadata
features yields a tokio-free binary. The audit confirms the
feature graph supports this; the guardrail script keeps it true.

## 9. Risks

### 9.1 Executor starvation

A long-running blocking call inside an async task starves the
worker pool. Mitigation: every CPU-heavy step on the async side
goes through `spawn_blocking` or `block_in_place` per section 5.
Phase 4 rolls out with a CI lint that greps for `std::fs::*` and
`rayon::scope` inside `async fn` in `crates/transfer/src/pipeline/`.

### 9.2 Cancellation semantics differ from sync abort

Sync transfers abort via `Result::Err`. An async task can be
cancelled by a parent dropping its future, leaving no opportunity
for cleanup unless the task holds a `Drop` guard. Mitigation:
explicit `tokio_util::sync::CancellationToken` propagation. The
`PipelineHandle` at
`crates/transfer/src/pipeline/async_pipeline.rs:151-155` already
carries one; Phase 4 extends to per-file tasks. The disk-commit
`Drop` impl finalises or unlinks the temp file. Phase 4 exit
criteria include a kill-test (section 7.2).

### 9.3 Deadlock across the bridge

Two failure modes: (a) a rayon worker calls `Handle::block_on`
while holding a buffer lease at
`crates/engine/src/local_copy/buffer_pool/pool.rs:459`, the
async task tries to take another buffer, both park; (b) an async
task does `block_in_place` while holding a tokio mutex, another
task takes the same mutex, the worker parks. Mitigation: section
5 forbids `block_on` from a rayon worker and forbids holding a
tokio resource across `block_in_place`. Phase 4 enforces with a
clippy `disallowed_methods` lint on `Handle::block_on` inside
`async_pipeline.rs`.

### 9.4 Livelock on retry storms

The async pipeline's retry path
(`async_pipeline.rs:161,178`) can re-enqueue a deterministically
failing job indefinitely. Mitigation: bounded retry per
`AsyncPipelineConfig.retry_enabled`; exponential backoff with cap;
the cancellation token at `async_pipeline.rs:147` aborts the
pipeline if backoff exceeds the per-session timeout.

### 9.5 Dual-runtime startup cost

Tokio at startup costs a thread pool, a reactor, and a timer
wheel - roughly 100-300 microseconds and a few MB of RSS. CLI
mode for local-only transfers does not need it. Mitigation: lazy
runtime construction; build only when the SSH transport, daemon,
or `rsync://` scheme is invoked. CLI uses one `current_thread`
runtime; multi-thread is reserved for the daemon. CI fails if
`oc-rsync --version` startup regresses by more than 10ms.

### 9.6 Bridge crate stability and io_uring composition

The migration depends on `flume` and
`tokio_util::sync::CancellationToken`. Both pinned in workspace
`Cargo.toml`. Phase rollout PRs require `cargo audit` clean. A
future RustSec advisory on either crate freezes the affected
phase until resolved.

io_uring lives in `fast_io` behind
`#[cfg(all(target_os = "linux", feature = "io_uring"))]` and
stays sync. The interaction surface is the disk-commit path that
Phase 4 migrates: an async task hands a buffer to a
`spawn_blocking` closure that calls into `fast_io::io_uring::*`.
The closure runs on the blocking pool, so the io_uring syscall
never pre-empts runtime workers. Tracked in #1595; see
`docs/design/io-uring-rayon-composition.md` for the boundary
diagram.

#### 9.6.1 Runtime choice: tokio-uring vs glommio vs sync io_uring

Three plausible runtimes can drive io_uring submissions; each
trades surface area for performance and ecosystem reach.

| Runtime | Pool model | Reactor | Ecosystem | Verdict |
|---------|------------|---------|-----------|---------|
| sync io_uring (today) | rayon + blocking pool | none, direct submit/wait | tokio-agnostic; works on any kernel >= 5.6 | keep |
| `tokio-uring` | tokio current-thread per worker | thread-local ring driven by tokio reactor | requires the current-thread runtime, not multi-thread | rejected for default daemon |
| `glommio` | one runtime per CPU, shard-nothing | per-shard ring, thread-pinned | separate executor; no tokio interop | rejected for workspace |

The sync io_uring path stays the default for three reasons. First,
it is already shipping behind `fast_io`'s feature gate and is
exercised by the criterion suite. Second, a `spawn_blocking`
boundary on the disk-commit path lets the multi-thread tokio
runtime keep its workers free for accept and codec work; the
blocking pool absorbs syscall latency. Third, the sync surface
keeps `fast_io` tokio-free, which the Phase 0 guardrail enforces.

`tokio-uring` is rejected as the default because it forces the
current-thread runtime, which conflicts with the daemon's need to
serve thousands of connections from a multi-thread pool. Adoption
would split the codebase: daemon on multi-thread, file I/O on
current-thread, with another bridge in between. We may revisit if
upstream tokio merges multi-thread io_uring; until then a single
optional `async-io-uring` feature could expose `tokio-uring` for
single-threaded callers without changing the default.

`glommio` is rejected because its shard-nothing model assumes one
runtime per CPU and rules out cross-thread channel use, which is
how the receiver pipeline (`run_pipeline` at
`async_pipeline.rs:137`) feeds disk commits. A glommio adoption
would require rewriting the entire pipeline around per-shard
queues, well beyond the scope of this plan.

The end-state: tokio multi-thread runtime owns scheduling, rayon
owns CPU, sync io_uring (driven from `spawn_blocking`) owns
fd-bound I/O. No second runtime ships in the default binary.

### 9.7 Dependency bloat

Tokio plus `tokio-util` plus `flume` plus `crossbeam-queue` plus
`futures-util` (transitive via tokio) adds roughly 70 transitive
crates and ~1.2 MB to a release binary on x86_64 Linux. The
audit at `docs/audits/tokio-dependency-boundary-2026.md` confirms
the seven-crate boundary keeps this contained: 18 workspace
crates ship without tokio, and the binary footprint of
`--no-default-features` builds is unchanged.

Mitigations:

- The Phase 0 guardrail script keeps the boundary at seven crates;
  any drift fails CI.
- `cargo bloat` is run on every Phase 2-5 rollout PR. The
  threshold is +10% on the release binary's `.text` section; a
  larger jump triggers a focused size audit.
- Optional features (`async-daemon`, `async-ssh`,
  `async-transfer`) compose; a distributor enabling only
  `async-daemon` does not pay for SSH-side or receiver-side async
  pulls.
- A `tokio-free` smoke build runs in CI on every PR
  (`--no-default-features --features metadata,perf`) to confirm
  the path stays viable for embedded distributors.

The accepted cost: when the user opts into the full async stack,
the binary grows by ~1.2 MB and link time grows by ~3 seconds on
warm caches. This is a one-time cost paid at build, not at
runtime.

### 9.8 Runtime selection at startup

oc-rsync runs in three modes (CLI client, CLI server-via-ssh,
daemon) and only some of them need a runtime. Naive adoption -
"build a multi-thread runtime in `main()`" - pays the startup
cost universally.

Failure modes:

- A short-lived `oc-rsync --version` invocation pays a 100-300
  microsecond runtime startup.
- Cron-driven local-only transfers (no SSH, no daemon, no
  `rsync://`) pay for a runtime they never use.
- The daemon needs multi-thread; the CLI client typically wants
  current-thread; building both forks code paths.

Mitigations:

- Lazy runtime construction. The runtime is built on first await,
  not in `main()`. The CLI startup path stays sync until the
  transport scheme is resolved.
- Mode-specific runtimes:

  | Caller | Runtime | Worker count |
  |--------|---------|--------------|
  | CLI local-only | none | n/a |
  | CLI over SSH (sync facade) | none | n/a |
  | CLI over SSH (async-ssh) | current-thread | 1 |
  | CLI over `rsync://` | current-thread | 1 |
  | Daemon | multi-thread | `num_cpus::get()` |

- One CI gate covers it: `oc-rsync --version` startup must not
  regress by more than 10ms (section 9.5). A second CI gate
  asserts the local-only transfer path never instantiates a
  tokio runtime, by checking that
  `tokio::runtime::Runtime::new` is not reached on the
  local-to-local benchmark trace.
- Documentation: the user-facing runbook lists the runtime cost
  per mode so distributors can make an informed choice.

The end-state: tokio runs only when an async-capable transport or
the daemon needs it; CLI local-only stays runtime-free.

## 10. Open Questions and Tracked Tasks

### 10.1 Open questions

- **Q1**: should the CLI's optional async path accept user jobs as
  `Future` factories, or stay sync at `main()`? Open until Phase 3
  ships and the embedded-SSH adoption curve is clear.
- **Q2**: when Phase 5 moves rayon and tokio to independent pool
  sizing, what is the heuristic? `num_cpus::get()` for rayon and
  half for tokio is a starting point;
  `docs/design/adaptive-thread-pool-sizing.md` is the vehicle.
- **Q3**: do we ever expose `tokio::sync::Notify` on a public
  API? Today no; reopens only if a future audit revisits the
  hot-path mutex policy.
- **Q4**: the embedded-SSH bridge is the only reason `rsync_io`
  ships an optional tokio dep
  (`crates/rsync_io/Cargo.toml:26,32`). The audit suggests
  hoisting it to a dedicated `embedded_ssh` crate; out of scope
  here, tracked separately.

### 10.2 Tracked tasks

- **#1590** tokio vs async-std: closed, superseded by #3706.
- **#1591** channel abstraction: Phase 1 prerequisite.
- **#1592** sync channel profiling: Phase 5 input
  (`docs/audits/transfer-hot-path-channel-overhead-static.md`).
- **#1593** async SSH evaluation: Phase 3 vehicle
  (`docs/audits/ssh-transport-async-evaluation.md`).
- **#1594** this plan.
- **#1595** async impact on io_uring: Phase 5 (section 9.6;
  `docs/audits/async-io-uring-interaction.md`).
- **#1737** async bandwidth limiter: shipped; ratified in Phase 1.
- **#1750** no sync/async API duplication: ratified.
- **#1751** `spawn_blocking` for rayon CPU work: Phase 5 (5.2).
- **#1779** original tokio scope audit: superseded by #3706.
- **#1934** async listener RFC: completed.
- **#1935** tokio listener implementation: Phase 2 promotes to default.
- **#3705** daemon scalability audit: cited in section 1.1.
- **#3706** tokio boundary re-verification: provides the
  seven-crate allow-list and the Phase 0 guardrail contract.

## 11. References

Audits:

- `docs/audits/tokio-dependency-boundary-2026.md` - PR #3706.
- `docs/audits/daemon-thread-per-connection-scalability.md` - PR #3705.

Adjacent design notes:

- `docs/design/async-channel-abstraction.md` - #1591.
- `docs/design/daemon-async-accept-sync-workers.md` - #1674.
- `docs/design/io-uring-rayon-composition.md` - #1283.
- `docs/design/multi-file-delta-apply-pipeline.md` - #1079.
- `docs/design/adaptive-thread-pool-sizing.md` - Phase 5 input.

Manifest pins: `Cargo.toml:24-35,107,188-189`. Per-crate gates are
listed in section 2.2. Source citations for every async surface
are in section 2.1; sync hot-path citations are in section 2.3.
