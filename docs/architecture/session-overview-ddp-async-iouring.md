# Session architectural overview: DDP, async transports, io_uring pools, CI expansion

This page is the architectural narrative for the work that landed in the
session preceding it: the parallel-deterministic-delete (DDP) pipeline,
the async SSH transport stack, the io_uring session and per-thread ring
pools, the tokio-based daemon listener, and the cross-platform CI
expansion. Each section names the modules that ship the behaviour and
points at the design docs that justify the shape. It is intentionally a
map, not a redesign; the design docs cited at the end of every section
are the source of truth.

## Executive summary

The shipped work pushes oc-rsync further along three independent axes
without changing the wire protocol or the user-visible CLI surface.
First, the deletion path is restructured into a two-phase pipeline
(parallel candidate compute fanned out across rayon, single emitter
draining in upstream order) so every `--delete-*` mode matches upstream
3.4.4 byte-for-byte by default while retaining internal parallelism.
Second, an opt-in async SSH transport (`async-ssh` feature) and an
opt-in tokio-based daemon listener (`async-daemon` feature) provide the
high-concurrency I/O surfaces required for fan-out workloads, layered
underneath the existing sync transfer engine via `spawn_blocking`
bridges. Third, io_uring grew two complementary pool primitives - a
bounded session ring pool keyed by `SessionId` and a thread-local pool
for pinned consumers - so daemon-burst sessions no longer pay
`io_uring_setup(2)` per connection and rayon-resident consumers can
submit without locks. CI grew a cross-OS feature matrix plus macOS and
Windows interop smoke harnesses so these surfaces are exercised on every
target platform before they are promoted to default.

---

## 1. DDP pipeline (parallel-deterministic delete)

DDP replaces the previous batched pre-transfer sweep
(`delete_extraneous_files` over a `HashMap<PathBuf, HashSet<OsString>>`)
with a two-phase model that produces byte-identical wall-clock event
order against upstream rsync 3.4.4 for every `--delete-*` mode. No new
user-visible flag controls this; parity is the default. The legacy
batched sweep no longer exists in the tree.

### Phase split

```
   flist segment #N -----------+
                               v
                  +---------------------------+
                  | compute_extras  (rayon)   |   pure read_dir + filter snapshot
                  +---------------------------+
                               |
                  publish DeletePlan(D)
                               v
   flist segment #N+1 ---------+
                  ...                                       (parallel fan-in)
                               v
                  +---------------------------+
                  |  DeletePlanMap            |   keyed by relative dir path
                  +---------------------------+
                               |
                               v
                  +---------------------------+
                  |  DirTraversalCursor       |   upstream depth-first + f_name_cmp
                  +---------------------------+
                               |
                               v
                  +---------------------------+
                  |  DeleteEmitter (single)   |   unlink + itemize + DeleteStats++
                  +---------------------------+
```

- **Phase 1 (parallel `compute_extras`)** runs on rayon workers, one
  per arriving INC_RECURSE flist segment. Each worker snapshots
  `read_dir`, subtracts the segment's entries, intersects with the
  per-directory `FilterChain` snapshot (including any `.rsync-filter`
  merge file loaded by `enter_directory`), sorts via the upstream
  `f_name_cmp` port (`protocol::flist::sort::compare_file_entries`),
  reverses to match `delete_in_dir()`'s decrementing iteration, and
  publishes the `DeletePlan` into `DeletePlanMap`. Workers never
  unlink, never emit output, and never mutate shared state beyond that
  one publish.
- **Phase 2 (single emitter)** owns every observable side effect.
  `DeleteEmitter` walks `DirTraversalCursor` in upstream order; for
  each directory it blocks until the plan is published, then issues
  `unlink` / `rmdir` (or `delete_dir_contents`-style recursion on
  `ENOTEMPTY`), emits `*deleting` via `writer.send_msg_info`, and
  updates `DeleteStats`. Hardlink cohorts come from a read-only
  `CohortIndex` snapshot built per segment, so cohort-aware deletes
  do not rerun stat under the emitter.

### Phase-mode integration

The emitter wiring observes the existing `--delete-before` /
`--delete-during` / `--delete-delay` / `--delete-after` selector
without code duplication:

- `--delete-before`: emitter drains the entire tree before the
  transfer loop begins.
- `--delete-during` (default for `--delete`): emitter interleaves
  with the transfer loop, draining each directory just as upstream's
  generator visits it.
- `--delete-delay`: emitter buffers per-segment plans and replays
  them in upstream order at finalisation, mirroring
  `do_delayed_deletions()`.
- `--delete-after`: emitter drains after the transfer loop completes.

The opt-in `--delete-strict-order` gate is gone (the flag and its 94
references were removed alongside the F3 sweep removal); parity is
unconditional.

### Source map

- Data structures and emitter:
  `crates/engine/src/delete/{mod.rs, plan.rs, plan_map.rs,
  traversal.rs, extras.rs, cohort_index.rs, emitter.rs, context.rs}`.
- Receiver hook publishing plans per segment:
  `crates/transfer/src/receiver/file_list.rs::receive_extra_file_lists`.
- Emitter dispatch from the receiver:
  `crates/transfer/src/receiver/directory/deletion.rs`.
- Upstream `f_name_cmp` port:
  `crates/protocol/src/flist/sort.rs::compare_file_entries`.

### Design references

- `docs/design/parallel-deterministic-delete.md` - the specification.
- `docs/design/ddp-f3-sweep-removal-readiness.md` - removal readiness
  audit for the legacy batched sweep.
- `docs/architecture/delete-during.md` - the consumer-facing
  architectural overview superseded by this section's pipeline view.
- Upstream: `generator.c::recv_generator`, `generator.c::delete_in_dir`,
  `generator.c::do_delete_pass`, `generator.c::do_delayed_deletions`,
  `delete.c::delete_item`, `delete.c::delete_dir_contents`.

---

## 2. Async SSH transport stack

The SSH client transport now has two parallel implementations sharing
one argv builder. The synchronous `SshConnection` (the production
default) and the async `AsyncSshTransport` (opt-in via the
`async-ssh` feature) both render identical bytes for a given
`(remote, args, config)` triple; only the process backing differs.

### Sync half (default)

- `crates/rsync_io/src/ssh/{builder.rs, connection.rs, connect.rs,
  aux_channel.rs}`.
- `SshCommand` wraps `std::process::Command`. The spawned `ssh` child
  inherits anonymous-pipe stdio (`Stdio::piped()` for stdin / stdout,
  socketpair-backed stderr on Unix per `aux_channel.rs`).
- Splits into `SshReader` / `SshWriter` halves. `SshChildHandle` Drop
  reaps the child to prevent zombies on early return.
- One process per transfer, matching upstream `do_cmd()`. Connection
  multiplexing is delegated to the user via OpenSSH `ControlMaster`.

### Async half (`--features async-ssh`)

- `crates/rsync_io/src/ssh/async_transport.rs` - `AsyncSshTransport`.
- Same `build_ssh_command` argv composition; `tokio::process::Command`
  replaces `std::process::Command`; stdin/stdout become
  `AsyncWrite`/`AsyncRead`. `kill_on_drop(true)` mirrors the sync
  path's reap-on-Drop guarantee.
- `split()` returns `(impl AsyncRead, impl AsyncWrite)` halves.
  Stderr stays inherited from the parent; an async stderr drain and
  async connect watchdog are deferred (the existing
  `-o ConnectTimeout=N` injection still applies).
- A compile-time argv-equivalence test
  (`execute_remote_rsync_argv_matches_sync_path`) guarantees the two
  paths stay byte-identical.

### Embedded `russh` half (`--features embedded-ssh`)

- `crates/rsync_io/src/ssh/embedded/` - in-process SSH client.
- `connect.rs::connect_and_exec` is async over a `russh::Channel`.
- `sync_bridge.rs::into_sync_halves` wraps the russh channel as
  `(std::io::Read, std::io::Write)` halves backed by a background
  tokio task with bounded `mpsc::channel(64)` queues, so the existing
  synchronous multiplex and transfer code drives a russh channel
  without being ported to async. This is the inverse of the channel
  adapter used by the daemon listener bridge.

### Selection matrix

| Build / feature                                    | Default | Transport used by the client remote path                                                  |
|----------------------------------------------------|---------|--------------------------------------------------------------------------------------------|
| `--no-default-features`                            | n/a     | `SshConnection` (sync subprocess), tokio not linked                                        |
| Workspace default                                  | yes     | `SshConnection` (sync subprocess)                                                          |
| `--features async-ssh`                             | no      | `AsyncSshTransport` (tokio-process subprocess); wired through the core remote dispatch     |
| `--features embedded-ssh`                          | no      | `russh` channel wrapped in `SyncAsyncBridge` / `into_sync_halves`                          |
| `--features async-ssh,embedded-ssh`                | no      | Caller selects; argv-equivalence and channel-bridge tests cover both                       |

Default builds stay tokio-free at the CLI level; the runtime is only
pulled in when one of the async or embedded gates is enabled.

### Design references

- `docs/design/ssh-transport-async-io-eval.md` - the #1593 evaluation
  (runtime flavour, FD bridge, cost, latency-sensitive sites,
  default-flip triggers).
- `docs/design/async-ssh-evaluation.md` and
  `docs/design/async-ssh-pipe-wrapper.md` - the prior wrapper choice
  (#1412).
- `docs/design/async-runtime-ssh-eval.md` - runtime selection (#1411).
- `docs/design/ssh-async-default-linux.md`,
  `docs/design/ssh-decouple-delta-from-socket-read.md`,
  `docs/design/ssh-explicit-backpressure-controls.md` - the follow-up
  triple for the eventual default flip.

---

## 3. io_uring topology: session pool vs per-thread pool

Two complementary primitives now coexist in
`crates/fast_io/src/io_uring/session_pool.rs`. Both target the same
problem (per-construction `io_uring_setup(2)` cost amortised across
many consumers) along orthogonal axes.

### `SessionRingPool` - bounded fleet, MPMC

- Fleet size defaults to `min(available_parallelism(), 16)`; each
  slot is a `Mutex<RawIoUring>` selected round-robin via a single
  relaxed `AtomicUsize`. `acquire()` returns a `RingLease` that
  `Deref`s to the ring and releases the mutex on Drop.
- Built for the daemon-session profile: many short-lived sessions
  sharing one pool keyed by `SessionId`. A back-to-back fan-in of
  100 sessions amortises a single `io_uring_setup` cost across the
  fleet instead of paying it 100 times.
- Contention point is the per-slot mutex, not the selector;
  registered buffers and fixed-file slots are sized per ring (see
  `docs/design/io-uring-adaptive-buffer-pool.md`).

### `ThreadLocalRingPool` - one ring per OS thread

- Lazily constructs one ring per thread on first acquire and holds
  it in TLS. The submit/reap fast path holds no lock because the
  ring never leaves its owning thread.
- Built for pinned consumers: the disk-commit thread, rayon workers,
  and any other thread with stable lifetime. SQE submissions stay on
  the same ring that owns the registered buffers and fixed-file
  table, so the per-thread fast path never loses its registered
  state to a steal.
- Work stealing across rings was rejected (`docs/design/iouring-per-thread-rings.md`
  section 3.3): stolen SQEs lose access to the originating ring's
  registered buffers and fixed-file table; the SQ-full path back-
  pressures instead.

### When to use which

| Consumer profile                                   | Pool                        |
|----------------------------------------------------|-----------------------------|
| Daemon connection bursts (short-lived sessions)    | `SessionRingPool`           |
| Disk-commit thread (one per transfer, pinned)      | `ThreadLocalRingPool`       |
| Rayon workers issuing fixed-buffer reads / writes  | `ThreadLocalRingPool`       |
| Per-file readers / writers (legacy)                | Existing `SharedRing` or migrate to `ThreadLocalRingPool` per the migration list in the design doc |

Existing single-owner `SharedRing` consumers
(`disk_batch`, `file_writer`, `file_reader`) keep working unchanged
and migrate one at a time as the bench evidence in #1410 / #4197
lands.

### Other io_uring primitives in this slice

- `linked_chain.rs` (PR #4296) - linked SQE chains for the
  read -> checksum -> write pipeline.
- `socket_factory.rs` / `socket_reader.rs` / `socket_writer.rs` - the
  TCP socket fast path; daemon-side wiring readiness is captured in
  `docs/design/iouring-socket-daemon-tcp-readiness.md`.
- `macos-kqueue-fast-io.md` covers the kqueue primitive on Darwin
  (the io_uring fast paths are Linux-only; macOS uses kqueue +
  `F_NOCACHE` writev as the parity surface).

### Design references

- `docs/design/iouring-session-ring-pool.md` - bounded MPMC pool spec.
- `docs/design/iouring-session-ring-pool-impl.md` - implementation
  plan.
- `docs/design/iouring-per-thread-rings.md` - per-thread primitive
  rationale; section 3.3 records the work-stealing rejection.
- `docs/design/io-uring-rayon-composition.md` and
  `docs/design/iouring-rayon-submission.md` - rayon composition.
- `docs/design/iouring-borrowed-slice-consumer.md` - re-entrancy
  hazards on borrowed completion slices.

---

## 4. Daemon async listener (hybrid model)

The daemon now exposes a tokio-based accept loop behind the
`async-daemon` Cargo feature
(`crates/daemon/Cargo.toml::async-daemon = ["dep:tokio"]`). The model
is intentionally hybrid: only the accept boundary is async; the
existing synchronous transfer worker continues to own the wire
protocol, filters, signature, and engine pipelines.

```
                              tokio runtime (rt-multi-thread, cap 8 workers)
                              +-----------------------------------------+
                              |                                         |
   incoming TCP ------------->| tokio::net::TcpListener::accept().await |
                              |                                         |
                              |   per-connection async task:            |
                              |     stream.into_std() + set_blocking    |
                              |     tokio::task::spawn_blocking(|| {    |
                              |         run_sync_worker(stream, ...)    |
                              |     }).await                            |
                              +-----------------------------------------+
                                                |
                                                v
                              +-----------------------------------------+
                              | blocking pool (size = max_conns + slack)|
                              |   existing sync transfer pipeline       |
                              |   (protocol, engine, transfer, filters) |
                              +-----------------------------------------+
```

- Module: `crates/daemon/src/daemon/async_session/{mod.rs, listener.rs,
  session.rs, shutdown.rs}`.
- The runtime is a multi-thread tokio runtime with worker count
  `min(available_parallelism(), 8)`. Shutdown rides the existing
  `Ctrl-C` / SIGTERM handlers via `tokio::signal` plus a broadcast
  channel.
- The async path applies the same `max-connections` semaphore as the
  legacy thread-per-connection path; behaviour at saturation matches.
- Panic isolation: panics inside the blocking task surface via
  `JoinHandle::is_err()`; the listener logs and continues, mirroring
  the existing `catch_unwind` semantics.
- The legacy `std::thread::spawn` accept loop remains the production
  default. The async path graduates after two release cycles of green
  CI plus interop runs.

### Why hybrid

The blocking transfer engine is rayon-parallel and 100% sync today.
Async-colouring `protocol`, `engine`, `transfer`, and `checksums`
would be a long migration with no measured throughput win at the
single-session level. The accept boundary is where async wins (many
concurrent connections, signal handling, idle low-cost waits); the
transfer body is where sync wins (blocking syscalls, rayon, no
reactor starvation hazards). Splitting at exactly that boundary keeps
both.

### Design references

- `docs/design/daemon-async-runtime-choice.md` - tokio vs async-std vs
  threaded, the runtime selection.
- `docs/design/daemon-async-accept-sync-workers.md` - the hybrid
  model rationale.
- `docs/design/daemon-tokio-async-listener-impl.md` - the
  implementation plan.
- `docs/design/async-migration-plan.md` - the long-term roadmap.
- `docs/design/tokio-spawn-blocking-rayon.md` - the rayon bridge
  surface (`rayon_bridge`, threshold short-circuit, `JoinError`
  mapping).

---

## 5. Cross-platform CI expansion

CI grew along two axes: a cross-OS feature-flag matrix for the rows
the audit flagged as OS-agnostic but Linux-only, and dedicated
macOS / Windows interop smoke harnesses against the platform's native
upstream rsync packaging.

### New matrix rows

`.github/workflows/_test-features.yml::feature-flags-cross-os` runs
the following rows on `ubuntu-latest`, `macos-latest`, and
`windows-latest` (3 OS x 4 rows = 12 jobs):

| Row name              | Scoped crates                                            | Feature(s)            |
|-----------------------|----------------------------------------------------------|-----------------------|
| `async`               | `daemon`, `core`, `protocol`, `engine`                   | `async`               |
| `tracing`             | `daemon`, `core`, `engine`                               | `tracing`             |
| `serde`               | `logging`, `protocol`, `flist`                           | `serde`               |
| `concurrent-sessions` | `daemon`                                                 | `concurrent-sessions` |

Linux-only rows (`io_uring`, `copy_file_range`, the crypto / deflate
backends) stay in the `feature-flags-linux` matrix; they overlap with
the per-OS `--all-features` jobs already in `ci.yml`.

### New interop jobs

- `interop (macOS)` - `.github/workflows/_interop-macos.yml` runs
  `tools/ci/run_interop_smoke.sh` against Homebrew's current upstream
  rsync (>= 3.4.x). Scenarios: baseline upstream local copy, push,
  pull, quick-check no-op, delta both directions, `--list-only`
  parity. Required check.
- `interop (Windows, best-effort)` -
  `.github/workflows/_interop-windows.yml` validates the
  `oc-rsync.exe` binary against MSYS2/Cygwin upstream rsync for
  push / pull / delta. Marked `continue-on-error` until baseline
  parity is green; promotes to required after that.

### macOS-specific additions

The `macos-test` matrix now also runs the `metadata` and `apple-fs`
crates (`-p metadata -p apple-fs`) on every toolchain row, covering
the Darwin `acl_exacl` branch, the macOS timestamp path
(`crates/metadata/src/apply/timestamps.rs`), and the AppleDouble
round-trip + resource-fork pipeline. Tests requiring root self-skip
via `geteuid()`; xattr-dependent tests probe support and skip on
filesystems that lack it.

### Required vs informational

Per `CLAUDE.md`, the gating required checks are: `fmt+clippy`,
`nextest (stable)`, `Windows (stable)`, `macOS (stable)`,
`Linux musl (stable)`, plus the macOS interop smoke harness. The new
cross-OS feature matrix and the Windows interop job are
informational until they accumulate two release cycles of green
runs, after which they flip to required.

### Design references

- `docs/audits/cross-platform-ci-coverage.md` - the gap audit driving
  this expansion.
- `docs/audits/cross-platform-parity-matrix.md` - the code-side
  parity matrix.
- `docs/audits/windows-acl-xattr-ci-matrix.md` - Windows
  ACL / xattr scope.

---

## 6. How the pieces fit together

A pull transfer from a remote SSH endpoint, with `--delete-during`
and `async-ssh`, executes the following pipeline:

1. `AsyncSshTransport::execute_remote_rsync` spawns the `ssh` child
   via tokio; `split()` hands the receiver thread an
   `(AsyncRead, AsyncWrite)` pair that is bridged into the sync
   multiplex layer via the existing channel adapter (the inverse of
   `embedded::sync_bridge::into_sync_halves`).
2. The receiver consumes INC_RECURSE flist segments. For each
   `FLAG_CONTENT_DIR` directory in a segment,
   `receive_extra_file_lists` posts a `compute_extras` job to rayon;
   the resulting `DeletePlan` lands in `DeletePlanMap`.
3. The `DeleteEmitter` (single thread) walks `DirTraversalCursor` in
   upstream order, blocks until each plan is ready, issues
   `unlink` / `rmdir` / recursion, emits `*deleting` via the
   multiplex writer, and updates `DeleteStats`.
4. The disk-commit thread owns a `ThreadLocalRingPool` ring for
   io_uring submissions; the per-file writer side reuses
   `SharedRing` until its migration to `ThreadLocalRingPool` lands.
5. On a daemon endpoint compiled with `--features async-daemon`,
   the accept boundary is tokio (`SessionRingPool` per-session
   amortises io_uring setup) while the transfer body runs on the
   blocking pool exactly as it does today.

Every step above is covered by either the design docs cited in
sections 1-4 or the CI matrices in section 5. None of the pieces
require coordinated rollout: DDP is on by default, async SSH and the
async daemon are opt-in, the io_uring pools are additive primitives
that migrate consumers one at a time, and the CI expansion is
infrastructure-only.

---

## 7. Index of design and audit documents

| Topic                                  | Document                                                                |
|----------------------------------------|-------------------------------------------------------------------------|
| DDP specification                      | `docs/design/parallel-deterministic-delete.md`                          |
| DDP F3 sweep removal readiness         | `docs/design/ddp-f3-sweep-removal-readiness.md`                         |
| Delete architecture (consumer view)    | `docs/architecture/delete-during.md`                                    |
| SSH transport async I/O evaluation     | `docs/design/ssh-transport-async-io-eval.md`                            |
| Async SSH pipe wrapper                 | `docs/design/async-ssh-pipe-wrapper.md`                                 |
| Async SSH evaluation                   | `docs/design/async-ssh-evaluation.md`                                   |
| Async runtime SSH evaluation           | `docs/design/async-runtime-ssh-eval.md`                                 |
| SSH async default flip (follow-up)     | `docs/design/ssh-async-default-linux.md`                                |
| SSH decouple delta from socket read    | `docs/design/ssh-decouple-delta-from-socket-read.md`                    |
| SSH explicit backpressure controls     | `docs/design/ssh-explicit-backpressure-controls.md`                     |
| io_uring session ring pool spec        | `docs/design/iouring-session-ring-pool.md`                              |
| io_uring session ring pool impl plan   | `docs/design/iouring-session-ring-pool-impl.md`                         |
| io_uring per-thread rings              | `docs/design/iouring-per-thread-rings.md`                               |
| io_uring rayon composition             | `docs/design/io-uring-rayon-composition.md`                             |
| io_uring rayon submission              | `docs/design/iouring-rayon-submission.md`                               |
| io_uring borrowed-slice consumer       | `docs/design/iouring-borrowed-slice-consumer.md`                        |
| io_uring socket daemon TCP readiness   | `docs/design/iouring-socket-daemon-tcp-readiness.md`                    |
| macOS kqueue fast I/O                  | `docs/design/macos-kqueue-fast-io.md`                                   |
| Daemon async runtime choice            | `docs/design/daemon-async-runtime-choice.md`                            |
| Daemon async accept + sync workers     | `docs/design/daemon-async-accept-sync-workers.md`                       |
| Daemon tokio async listener impl       | `docs/design/daemon-tokio-async-listener-impl.md`                       |
| Async migration plan (roadmap)         | `docs/design/async-migration-plan.md`                                   |
| Tokio spawn_blocking + rayon bridge    | `docs/design/tokio-spawn-blocking-rayon.md`                             |
| Cross-platform CI coverage             | `docs/audits/cross-platform-ci-coverage.md`                             |
| Cross-platform parity matrix           | `docs/audits/cross-platform-parity-matrix.md`                           |
| Windows ACL / xattr CI matrix          | `docs/audits/windows-acl-xattr-ci-matrix.md`                            |
