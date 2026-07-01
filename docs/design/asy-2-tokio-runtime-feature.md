# ASY-2: `tokio-transfer` cargo feature design

Status: Design (follow-up to ASY-1 `docs/audits/asy-1-threading-model.md`).
Companion to ASY-3..6 (rollout) and ASY-7..12 (per-boundary
conversions). Scope: define the cargo feature flag that swaps the
threaded Generator/Receiver/disk-commit pipeline for a tokio-driven
one, and resolve the contract questions implementation needs before
any `.rs` changes land. Implementation lives under ASY-3+.

## 1. The question

ASY-1 mapped 12 candidate async boundaries and 8 sync semantics that
must not regress. We need a single cargo gate that controls whether
those boundaries flip to `.await` or stay on `std::thread`. The gate
must compile out cleanly when off (zero tokio in the transfer hot
path), host the receiver pipeline on tokio when on without breaking
wire-byte parity or forking `core::session()`, and compose with the
pre-existing `async` features in `core`, `transfer`, and `daemon`
rather than fragmenting the feature graph further.

## 2. Feature name and scope

### 2.1 Name

`tokio-transfer`.

The existing `async` feature is overloaded - it gates the
`async_pipeline` skeleton in `transfer`, `tokio` for `embedded-ssh` /
`async-ssh` in `core`, and the hybrid accept loop in `daemon`. Adding
more behaviour under the same name produces a flag that "enables
async" but means three things depending on crate. `tokio-transfer`
names exactly what it does: replace the transfer pipeline's threaded
scheduling with tokio. It depends on `async` (which provides the
`tokio` dep) but is not equivalent to it.

### 2.2 Workspace crates that gain the gate

| Crate       | Feature added       | Forwards to                                | Notes |
|-------------|---------------------|--------------------------------------------|-------|
| root `bin`  | `tokio-transfer`    | `protocol/tokio-transfer`, `core/tokio-transfer`, `transfer/tokio-transfer`, `daemon/tokio-transfer` | Top-level switch. Implies `async`. |
| `core`      | `tokio-transfer`    | `transfer/tokio-transfer`, `dep:tokio`     | Adds the runtime-ownership shim inside `core::session()`. |
| `transfer`  | `tokio-transfer`    | `dep:tokio`, `dep:tokio-util`, `async`, `protocol/tokio-transfer` | Compiles the tokio receiver/generator paths and the async multiplex read leaf. |
| `protocol`  | `tokio-transfer`    | `dep:tokio` (`io-util` only)               | See the 2026-07-01 amendment below. Optional async multiplex read leaf (`recv_msg_into_async`); no other change when off. |
| `daemon`    | `tokio-transfer`    | `async-daemon`, `core/tokio-transfer`      | Lets the hybrid listener hand connections directly to the tokio receiver instead of `spawn_blocking`. |

Crates NOT touched: `engine`, `fast_io`, `signature`,
`filters`, `metadata`, `checksums`, `compress`. They stay sync and are
called from `spawn_blocking` islands. ASY-9 decides whether
delta-apply ever becomes async natively. (`protocol` is amended below;
`compress` follows in a later rung.)

## 2.3 Amendment (2026-07-01): async read/write leaves in protocol/compress

The original §2.2 placed `protocol` and `compress` in the "not touched"
set, on the assumption that a `transfer`-only rung could reach every
async boundary. **The ASY-7 receiver scoping result
(`docs/design/asy-7-receiver-tokio-prototype.md`) disproves that
assumption.** ASY-7 §2-3 traced boundary #4 (the receiver's delta-token
wire read) to its leaf and found the real socket-read leaf is
`protocol::recv_msg_into` (`crates/protocol/src/multiplex/io/recv.rs`),
reached through `MultiplexReader::read`. A genuine `.await` on that read
cannot exist while the leaf itself is a synchronous `fn`: Rust cannot
`.await` from a sync function, and hosting the sync leaf under
`block_on` yields zero async benefit (ASY-7 §3.1, §3.4). The boundary is
therefore not separable inside `transfer` alone - the leaf lives in
`protocol`.

**Amendment.** Behind the default-off `tokio-transfer` feature,
`protocol` (and later `compress`) MAY expose async leaf variants that
read from `tokio::io::AsyncRead` / write to `tokio::io::AsyncWrite`,
alongside - never replacing - the existing sync leaves. Constraints:

- **Additive and default-off.** With `tokio-transfer` off, `protocol`
  pulls no tokio dependency and is byte-identical to today. The sync
  leaves (`recv_msg`, `recv_msg_into`, `send_msg`, ...) are unchanged.
- **No forked parser.** The async leaf and the sync leaf share the pure
  framing/decode logic (header decode, payload buffer preparation,
  truncation errors) via one internal seam, so they can never diverge on
  wire interpretation. The variants differ only in how bytes are pulled
  (`.await` vs blocking).
- **Unsafe-free.** `protocol` keeps `#![deny(unsafe_code)]`; the async
  path is all safe (`AsyncReadExt`).
- **Unwired until its consuming rung.** The first leaf
  (`recv_msg_into_async`, this amendment) is not connected to
  `MultiplexReader` or the receiver; it is the reviewable primitive that
  the coupled ASY-7-redo rung (ASY-7 §5) will consume once the demux,
  SPSC->mpsc swap, and disk-task restructure land together.
- **Parity is enforced in CI.** The `async-wire-parity` gate feeds
  identical wire bytes to both the sync and async leaves and asserts
  byte-identical parsed output frame-for-frame, including chunked
  delivery across `.await` points.

`compress`'s async token leaf (`decoder.recv_token` over an async
reader) and the multiplex demux itself follow the same pattern in later
rungs; this amendment establishes the precedent and the shared-seam
discipline.

## 3. Default state and rollout path

**Default: off** in all crates, including the root `[features]` table.
Opt-in for the entire duration of ASY-2..11.

The flip-to-default-on gate is ASY-12 and requires:

1. All 12 ASY-1 boundaries either converted to `.await` or explicitly
   classified as `spawn_blocking` islands with measured overhead under
   5% vs the threaded path.
2. `tools/ci/run_interop.sh` green on rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2
   with the feature on.
3. Golden wire-byte tests (`crates/protocol/tests/golden/`) bit-for-bit
   identical between threaded and tokio paths across a matrix that
   exercises every "Preserved" invariant from ASY-1.
4. Peak RSS within 10% of the threaded path on the `rsync-profile`
   100k-file benchmark.
5. Documented rollback path (section 9).

Until ASY-12, `tokio-transfer` is documented as **experimental** in
help text and release notes, mirroring how `parallel-receive-delta`
was scaffolded - but without repeating PIP-7's mistake of defaulting
the scaffold on.

## 4. Public API surface

`core::session()` and `CoreConfig` stay byte-identical regardless of
the feature flag. This is the only non-negotiable shape rule. The
contract:

- The CLI does not pick the pipeline. `cli::frontend` builds the same
  `CoreConfig` either way and calls `core::session(cfg)`.
- Inside `core::session()`, a `#[cfg(feature = "tokio-transfer")]`
  branch picks the pipeline driver. When off, the existing threaded
  driver runs unchanged. When on, the tokio driver runs.
- Both drivers return the same `Result<TransferStats, CoreError>`.
- No tokio types appear in any public signature of `core`, `transfer`,
  or `engine`. Tokio is an internal implementation detail.

Embedders (`crates/embedding`, integration tests, library consumers)
stay unaware of the runtime choice. The async pipeline skeleton at
`crates/transfer/src/pipeline/async_pipeline.rs` is the seed; ASY-3
promotes it from `#[cfg(feature = "async")]` to
`#[cfg(feature = "tokio-transfer")]` and wires it into
`run_server_with_handshake`.

## 5. Runtime ownership

The tokio runtime is owned by one of two entry points, decided at
runtime by who called `core::session()`:

1. **Daemon path:** the existing multi-thread runtime built in
   `crates/daemon/src/async_listener.rs::run_hybrid_listener`. When
   `tokio-transfer` is on, the daemon replaces
   `spawn_blocking(move || worker(stream))` with
   `tokio::spawn(async_worker(stream))` driving the tokio receiver
   directly. One runtime per daemon process.
2. **CLI / SSH path:** `core::session()` probes via
   `tokio::runtime::Handle::try_current()`. If a runtime exists (e.g.
   the SSH transport's `current_thread` runtime in
   `async_ssh_transport.rs:245`), it adopts that handle. If not, it
   builds a `Builder::new_current_thread()` runtime scoped to the
   session and `block_on`s the pipeline future. The multi-threaded
   flavour is reserved for the daemon.

Rule: no crate below `core` may build a runtime. `transfer`, `engine`,
and `fast_io` only ever see a `Handle` passed down from `core`. ASY-1
candidate boundary 12 (the `spawn_blocking(run_blocking_server)`
island) dissolves because the server body itself becomes async.

## 6. `spawn_blocking` policy

Mapped against ASY-1's 12 candidate boundaries:

| ASY-1 # | Boundary                                            | Policy under `tokio-transfer` |
|---------|-----------------------------------------------------|-------------------------------|
| 1       | Generator wire `reader.read`                        | `.await` on `tokio::io::AsyncRead` wrapper around the transport. |
| 2       | Generator wire `writer.flush` / `write_all`         | `.await` on `tokio::io::AsyncWrite` wrapper. |
| 3       | Generator basis-file `read_to_end` / `MapFile`      | `spawn_blocking` island. `MapFile` is mmap-backed and page-fault-driven; not safely awaitable. ASY-9 reconsiders. |
| 4       | Receiver wire `reader.read` for delta tokens        | `.await` (same transport wrapper as #1). |
| 5       | Receiver `writer.flush` gated on flushed_pending=0  | `.await`. Multiplex flush-before-block invariant (ASY-1 "Preserved") must hold; we flush before any `.await` on a read. |
| 6       | `spsc::Sender::send` spin-wait                      | Replaced by `tokio::sync::mpsc::Sender::send().await`. SPSC ring goes away in the tokio path. |
| 7       | `spsc::Receiver::recv` spin-wait                    | Replaced by `tokio::sync::mpsc::Receiver::recv().await`. |
| 8       | `find_basis_file_with_config` rayon worker          | Stays on rayon under `spawn_blocking`. Rayon-on-tokio crossings use a single `spawn_blocking` per signature batch, not per file. |
| 9       | `disk_commit::process_file` blocking write + fsync  | `spawn_blocking` island. Disk-commit thread becomes a `spawn_blocking` task with the same single-owner discipline. |
| 10      | `fast_io::IoUringDiskBatch::submit_and_wait`        | `spawn_blocking` until ASY-9 lands a `tokio-uring` driver. See section 8. |
| 11      | Daemon `TcpListener::accept`                        | Already async under `async-daemon`. `tokio-transfer` does not change this. |
| 12      | SSH `spawn_blocking(run_blocking_server)`           | Dissolved. The server body becomes async; the paired `std_mpsc` boundary is removed. |

Rule of thumb: any boundary that touches mmap, ioctl, fsync, or a
syscall-backed wrapper crate (`exacl`, `xattr`, `nix`) stays in
`spawn_blocking`. Any boundary that is a `Read`/`Write` on a socket or
pipe becomes `.await`.

## 7. Wire-byte parity invariant

**The tokio path must produce byte-identical wire output to the
threaded path for every supported transfer.** Non-negotiable; the
single rollback trigger for the feature flag.

Test contract (added under ASY-5):

1. **Golden parity:** every golden in
   `crates/protocol/tests/golden/` is rerun twice in CI - feature off
   and on. Bit-for-bit identical output required. Any diff fails the
   build.
2. **Interop parity:** `tools/ci/run_interop.sh` runs end-to-end
   against rsync 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 with the feature on,
   in addition to the default off run.
3. **Capture-replay:** the harness in
   `docs/design/capture-replay-harness.md` records a wire trace from
   the threaded path and replays it under the tokio path to compare
   multiplex frame ordering, NDX request dispatch order, and per-file
   commit order.
4. **Preserved-invariants suite:** one targeted test per bullet in
   ASY-1 "Preserved" (8 tests). Each must pass under both pipelines.

Tokio is not allowed to reorder anything that the threaded path
serialises: in-order NDX dispatch, in-order disk-commit, and the
phase 1 -> phase 2 redo barrier.

## 8. io_uring intersection

`fast_io::IoUringDiskBatch::submit_and_wait(1)` is synchronous. Under
`tokio-transfer` the disk-commit worker becomes a `spawn_blocking`
task that owns its ring, preserving the single-owner discipline
(ASY-1 "Preserved"). Same dispatch shape as today; the parent is a
tokio task instead of an `std::thread`. Throughput is expected
neutral because the ring still batches submissions itself.

A native async driver via `tokio-uring` would let the disk-commit
worker `.await` ring completions directly. **Decision: punt to
ASY-9.** `tokio-uring` is single-threaded (one runtime per OS thread)
which conflicts with the daemon's multi-thread model; the shared-ring
bottleneck (`project_io_uring_shared_ring_bottleneck.md`) and
per-thread ring work (IUR-2/IUR-3) must land first or we lock the
design to a shape the lower layer is moving away from. ASY-9 revisits
once IUR-3 is shipped and benchmarked.

## 9. Rollback criteria

Revert the feature (kept compiled, defaulted-off, marked deprecated,
removed in a follow-up release) on any of:

- **Wire-byte divergence** in golden or interop CI that cannot be
  reproduced and fixed within two release cycles.
- **Throughput regression** > 10% on the `rsync-profile` 100k-file
  benchmark vs the threaded path, persistent across two runs.
- **Peak RSS regression** > 15% on the same benchmark.
- **Tokio CVE** affecting `rt-multi-thread`, `io-util`, `net`, or
  `sync` without a patch within 14 days. `async-daemon` has the same
  exposure; both revert together.
- **Cross-platform breakage** on macOS or Windows that the threaded
  path does not exhibit. If tokio's Windows IOCP integration
  interferes with `fast_io::iocp`, revert before platform CI drifts.

Rollback is a one-line change in the root `Cargo.toml`. No public API
shift because section 4 forbids any.

## 10. Open questions for ASY-3..6

Implementation cannot start until these resolve:

1. **ASY-3 (runtime construction):** confirm
   `Handle::try_current()` adoption is safe when called from inside a
   `block_on` on a `current_thread` runtime. Tokio docs say yes;
   needs a probe test.
2. **ASY-4 (transport wrappers):** design the
   `tokio::io::AsyncRead` / `AsyncWrite` wrapper around the existing
   `rsync_io` transports without doubling buffering.
3. **ASY-5 (test contract):** decide how the golden harness runs
   "both pipelines" without doubling CI wall-clock.
4. **ASY-5 (capture-replay):** confirm the harness in
   `docs/design/capture-replay-harness.md` can record from the
   threaded path and assert against the tokio path without
   protocol-level fuzzing noise.
5. **ASY-6 (adopt-or-skip):** floor benchmark uplift below which
   ASY-2..12 is abandoned. Proposal: < 5% improvement on the 100k-file
   benchmark = abandon. Needs explicit sign-off before ASY-3 spends
   implementation effort.
6. **Rayon-on-tokio:** signature batch worker uses
   `rayon::ThreadPool::install`. Confirm wrapping the batch in one
   `spawn_blocking` does not starve the tokio blocking pool under
   daemon concurrent-connection load.
7. **`async-ssh` interaction:** `core/async-ssh` and
   `core/tokio-transfer` both pull tokio. Confirm no feature-graph
   cycles. Likely answer: `tokio-transfer` does not depend on
   `async-ssh`; both depend on `async`.
8. **Drop-order safety:** the threaded path's
   `PipelinedReceiver::shutdown -> JoinHandle::join` becomes
   `JoinHandle::await`. Confirm tokio's abort-on-drop does not
   silently lose in-flight commits if the future is cancelled
   mid-transfer.

## 11. Cross-references

- `docs/audits/asy-1-threading-model.md` - source map.
- `docs/design/async-migration-plan.md` - earlier sketch; this doc
  supersedes its feature-flag section.
- `docs/design/daemon-async-runtime-choice.md` - resolves async-std
  vs tokio. `tokio-transfer` inherits that answer.
- `docs/audits/async-daemon-listener.md`,
  `docs/audits/async-ssh-transport.md` - hybrid listener and SSH
  transport that `tokio-transfer` builds on.
- `docs/design/async-io-uring-impact.md`,
  `docs/audits/async-io-uring-interaction.md` - background for the
  ASY-9 punt.
- `project_no_async_threaded_only.md`,
  `project_io_uring_shared_ring_bottleneck.md`,
  `project_parallel_interop_parity_gap.md` - standing constraints
  the feature inherits.
