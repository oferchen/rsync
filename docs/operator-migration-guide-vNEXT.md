# Operator migration guide: vNEXT (DDP + async stack)

This guide is for operators upgrading from the previous oc-rsync release
to vNEXT, the version that ships the parallel-deterministic-delete
(DDP) pipeline, the opt-in async SSH transport, the opt-in tokio-based
daemon listener, and a small set of Cargo feature flags that gate the
new performance surfaces.

It calls out the behavioural changes vs prior versions, the flags that
moved or disappeared, the opt-in switches that are new, the CI matrix
changes that mean macOS and Windows are now first-class targets, and
the rollback procedure for pinning to the previous release if a
regression surfaces.

Architectural context for everything below is in
[`docs/architecture/session-overview-ddp-async-iouring.md`](architecture/session-overview-ddp-async-iouring.md).
The DDP specification is in
[`docs/design/parallel-deterministic-delete.md`](design/parallel-deterministic-delete.md).
The async daemon and async SSH evaluations are in
[`docs/design/daemon-async-runtime-choice.md`](design/daemon-async-runtime-choice.md)
and
[`docs/design/ssh-transport-async-io-eval.md`](design/ssh-transport-async-io-eval.md).

## 1. Wire-format compatibility

No protocol changes. vNEXT speaks protocol 32, byte-for-byte
identically with the previous release and with upstream rsync 3.4.4.

- Existing clients can connect to vNEXT servers and daemons unchanged.
- Existing servers and daemons can accept vNEXT clients unchanged.
- Mixed-version fleets are fully supported. There is no flag day.
- Capability strings, multiplex frames, file-list segments, signature
  blocks, token streams, `MSG_*` envelopes, and exit codes are
  unchanged from the previous release.
- The golden byte-fixture suite under
  `crates/protocol/tests/golden/` continues to pass against upstream
  rsync 3.0.9, 3.1.3, and 3.4.1.

If your monitoring relies on parsing oc-rsync output, the only
observable change is the wall-clock ordering of `*deleting` itemize
lines under `--delete-during` (section 2). Everything else - message
text, error format, exit codes, role trailers, statistics summary -
is unchanged.

## 2. Delete-mode semantics change

vNEXT replaces the previous batched pre-transfer delete sweep with a
two-phase pipeline (parallel candidate compute on rayon, single
emitter draining in upstream order). The final on-disk state is
identical; what changes is the wall-clock event order and the
interleave with the transfer loop.

### What changed per mode

| Mode               | Previous behaviour                                              | vNEXT behaviour                                                                 |
|--------------------|-----------------------------------------------------------------|--------------------------------------------------------------------------------|
| `--delete-before`  | Single batched sweep before the transfer loop.                   | Single emitter drains the whole tree before the transfer loop. Same placement, deterministic per-directory order. |
| `--delete-during`  | Single batched sweep before the transfer loop; itemize order non-deterministic above 64 entries. | Per-directory interleave with the transfer loop, matching upstream `generator.c::generate_files()` byte-for-byte. |
| `--delete-delay`   | Same batched sweep, just deferred placement.                    | Per-segment plans buffered, replayed at finalisation in upstream order, mirroring `do_delayed_deletions()`. |
| `--delete-after`   | Batched sweep after the transfer loop.                          | Single emitter drains after the transfer loop, deterministic per-directory order. |

### Operator impact

- **Final state is unchanged.** Any operator who only cares about
  whether files are removed at the end of the transfer sees no
  difference. Counts in the statistics summary are unchanged.
- **`*deleting` itemize order changes** under `--delete-during` and
  becomes deterministic in every mode. Log scrapers that depend on
  the previous arbitrary ordering must be updated to handle the new
  upstream-identical order. The order is now:
  per directory, entries in reverse `f_name_cmp` order; directories
  in upstream depth-first traversal order.
- **`--delete-during` now interleaves with transfers.** Previously the
  entire deletion sweep ran before any transfer. Now deletions and
  transfers happen per-directory as upstream does. If an operator's
  workflow assumed deletions complete before any data is written
  (rare; this was never documented), use `--delete-before` instead.
- **Filter chain snapshots are per-directory.** `.rsync-filter` merge
  files loaded by `enter_directory` for a subtree are now honoured by
  the deletion path for that subtree, matching upstream. Previously a
  single chain snapshot was taken at sweep start.

### What is unchanged

- `--max-delete` semantics, exit code, and ordering of the
  enforcement check.
- `DeleteStats` totals (files, dirs, symlinks, devices, specials) and
  the `NDX_DEL_STATS` wire frame in the goodbye phase (protocol >= 31).
- The `*deleting` itemize line format itself; only the order changes.
- `--ignore-errors`, `--force`, and `--protect-args` interaction with
  deletion.

## 3. `--delete-strict-order` removal

The opt-in `--delete-strict-order` / `--no-delete-strict-order` flags
introduced in the prior prerelease for #1940 are removed. Upstream
per-directory ordering is now the unconditional default for every
`--delete-*` mode.

### Migration

- If your invocation passed `--delete-strict-order`, remove it.
  The behaviour the flag selected is now the default and only
  behaviour.
- If your invocation passed `--no-delete-strict-order` to opt out of
  the strict-order path, remove it as well. The legacy batched sweep
  no longer exists; there is no off switch. The final on-disk state is
  unchanged either way, so this should be a no-op for any successful
  transfer.
- Scripts that pre-flight oc-rsync help text with `--help | grep` will
  no longer find `delete-strict-order`. Remove the check.

Background: the historical design at
[`docs/design/delete-during-strict-order-gate.md`](design/delete-during-strict-order-gate.md)
is marked SUPERSEDED. The replacement is the always-on DDP model in
[`docs/design/parallel-deterministic-delete.md`](design/parallel-deterministic-delete.md).

## 4. New opt-in feature flags

vNEXT introduces six Cargo feature flags that gate new performance
surfaces. None are enabled by default. None change wire bytes. All can
be combined.

### `async-ssh` (`core`, `rsync_io`)

- **What it does.** Replaces the synchronous `SshConnection`
  subprocess wrapper with an `AsyncSshTransport` built on
  `tokio::process::Command`. The argv handed to `ssh` is byte-identical
  (covered by `execute_remote_rsync_argv_matches_sync_path`).
- **When to opt in.** High-RTT links, fan-out clients that open many
  concurrent SSH connections, or workloads where the receiver disk
  write would otherwise serialise with the socket read. Expected
  wall-clock wins are tabulated in
  [`docs/design/ssh-transport-async-io-eval.md`](design/ssh-transport-async-io-eval.md)
  section 2 (8 to 20% on RTT-bound or rotational-disk transfers,
  thread-count reduction of 4x to 8x on fan-out workloads).
- **When to stay on the default.** LAN + SSD single-file transfers
  show no win and pay the tokio runtime cost (about 2 MiB RSS, plus
  worker threads). CLI invocations that already finish in a few
  hundred ms are not worth the runtime startup.
- **Build.** `cargo build --release --features async-ssh`.

### `async-daemon` (`daemon`)

- **What it does.** Adds a tokio-based accept loop alongside the
  thread-per-connection listener. The accept boundary is async; the
  transfer body still runs on the existing blocking pipeline via
  `spawn_blocking`. Same `max-connections` semaphore, same shutdown
  semantics, same panic isolation.
- **When to opt in.** Daemon deployments that expect high concurrent
  connection counts (hundreds of short-lived sessions, fan-out from
  CI fleets, mirror endpoints). The accept boundary scales better
  than `std::thread::spawn` per connection.
- **When to stay on the default.** Single-tenant daemons, low-volume
  endpoints, or anywhere the operator does not want a tokio runtime
  linked in. The threaded path is the production default and stays
  the default for at least two release cycles of green CI on the
  async path.
- **Build.** `cargo build --release --features async-daemon`.

### Parallel receive-delta (unconditional)

- **What it does.** Fans out per-chunk strong-checksum verification
  across rayon workers while serializing writes through a per-file
  reorder buffer. Verification scales with core count; writes stay
  deterministic.
- **Status.** Unconditionally compiled. The `parallel-receive-delta`
  feature flag was removed in PFF-7 after validation through full
  upstream interop (PIP-10.a), adversarial stress tests (PIP-10.b),
  mixed-size correctness (PIP-10.c), RSS overhead measurement
  (PIP-10.d), error-path parity (PIP-10.e), and a bake window with
  zero attributable regressions (PIP-9.f). There is no opt-out; the
  parallel path is the only path.

### `thread-slab-pool` (`engine`)

- **What it does.** Replaces the single-slot thread-local cache in
  front of `BufferPool` with a depth-bounded LIFO slab per thread
  (default 1 MiB byte cap). Cross-thread returns still fall through
  to the central overflow queue.
- **When to opt in.** Receiver or sender deployments running with
  more than about 32 worker threads per pool, where the central-queue
  cursor traffic on every buffer return becomes contention.
- **When to stay on the default.** Below 32 worker threads the
  single-slot path is at least as good and uses less idle memory.
  Steady-state memory grows by `N_threads * byte_cap`, so do not
  enable on memory-constrained endpoints.
- **Build.** `cargo build --release --features thread-slab-pool`.

### `ssh-socketpair-stderr` (`rsync_io`, experimental)

- **What it does.** Replaces the anonymous-pipe stderr channel of the
  SSH child with a `socketpair(AF_UNIX, SOCK_STREAM, 0)` constructed
  via `UnixStream::pair`. The parent end is a bidirectional socket that
  can be registered with `epoll`/`kqueue` (or tokio `AsyncFd`) and woken
  out-of-band via `shutdown(2)`, which is the seam SSE-4 uses to drive
  the drain off a tokio task instead of a dedicated thread per
  connection. The child still sees a plain stream of bytes on fd 2.
  Capture semantics, line forwarding to host stderr, and the bounded
  64 KiB ring buffer used by `stderr_output()` are unchanged.
- **When to opt in.** Linux endpoints that already build with
  `async-ssh` and want the SSH stderr drain integrated into the same
  tokio reactor as the wire path, instead of consuming a per-connection
  blocking thread; long-running fan-out clients that open many
  concurrent SSH children where the saved drain threads matter; and
  any deployment that wants the larger default kernel buffer
  (~208 KiB on Linux vs 64 KiB for pipes) and `shutdown(SHUT_RD)`
  as the wake primitive for the drain loop. macOS works too, with
  the same `UnixStream::pair` construction.
- **When to stay on the default.** Operators who prefer the simpler
  pipe semantics for debugging (a pipe is unidirectional and shows up
  as a single read-only fd in `lsof` / `procfs`); Windows endpoints,
  where the TCP-loopback shim is still in flight under SSE-5 and
  falls back to `Stdio::piped()` on any error; sync-only SSH
  deployments that do not link tokio, since the existing sync
  transport already uses the socketpair when available and gains
  nothing from the flag. The default-off ships exactly what `master`
  shipped before the SSE series.
- **Build.** `cargo build --release -p rsync_io --features ssh-socketpair-stderr`.
  Combine with `async-ssh` to actually exercise the async drain path:
  `cargo build --release --features "async-ssh" -p core` and
  `cargo build --release --features "ssh-socketpair-stderr" -p rsync_io`.
- **Design reference.** Rationale, cross-platform construction, and
  the SSE-3 through SSE-7 staging plan are in
  [`docs/design/socketpair-stderr-channel.md`](design/socketpair-stderr-channel.md)
  (#2371). The companion stderr-handling audit that motivated the
  series is in
  [`docs/audits/ssh-stderr-handling.md`](audits/ssh-stderr-handling.md)
  (#2370).

### `vmsplice` (`fast_io`, `transfer`, Linux only)

- **What it does.** Enables a Linux-only zero-copy disk writer that
  moves a userspace buffer to a regular file via `vmsplice(2)` +
  `splice(2)`. The trigger workload is kernel < 5.6 or io_uring
  disabled, large literal tokens, and a splice-capable filesystem.
- **When to opt in.** Linux endpoints where io_uring is unavailable
  (kernel < 5.6, or io_uring administratively disabled) and the
  workload sends large literal tokens to a splice-capable filesystem
  (tmpfs, ext4, xfs).
- **When to stay on the default.** Any non-Linux target. Any Linux
  endpoint with io_uring available - the io_uring path already
  delivers the same wins through a more general primitive.
- **Build.** `cargo build --release --features vmsplice`
  (Linux only; no-op on other targets).

### Receiver spill tunability (`SpillPolicy`)

The concurrent-delta receiver bounds its `ReorderBuffer` through a
process-wide `SpillPolicy` introduced in STN-1 (design) and STN-2
(struct). The default policy keeps the historical behaviour - no spill,
everything in memory, byte-equivalent to the previous release - so
existing operators see no behavioural change.

- **What it does.** When `threshold_bytes` is set, the receiver writes
  the oldest-eligible items in the reorder window to a tempfile once
  the in-memory footprint crosses the threshold and reloads them when
  they reach the head of the delivery order. The on-disk format is a
  length-prefixed binary payload; compression is opt-in.
- **Defaults.** `threshold_bytes = None` (spill disabled),
  `dir = None` (defers to **std::env::temp_dir**(3) via a 1 MiB
  spooled tempfile), `reclaim_mode = KeepInMemory`,
  `granularity = WholeBatch`, `compression = None`. The defaults
  table and rationale are in
  [`docs/design/spill-policy-public-api.md`](design/spill-policy-public-api.md)
  section 2.
- **Environment variables.** All five `SpillPolicy` fields are
  reachable through `OC_RSYNC_SPILL_*` env vars; precedence (highest
  wins) is CLI flag > env var > programmatic policy > default.

| Variable | Maps to | Accepted values |
|----------|---------|-----------------|
| `OC_RSYNC_SPILL_THRESHOLD_BYTES` | `threshold_bytes` | Integer with optional `K`/`M`/`G` suffix (case-insensitive, base 1024). Empty string clears. `0` is rejected. |
| `OC_RSYNC_SPILL_DIR` | `dir` | Absolute or relative path. Created on first spill via `create_dir_all`. |
| `OC_RSYNC_SPILL_RECLAIM` | `reclaim_mode` | `keep` (default) or `re-spill`. |
| `OC_RSYNC_SPILL_GRANULARITY` | `granularity` | `whole-batch` (default) or `per-item`. |
| `OC_RSYNC_SPILL_COMPRESSION` | `compression` | `none` (default), `zstd`, or `zstd:LEVEL` where `LEVEL` is in `[-22, 22]`. |

- **When to override.**
  - *Memory-constrained receivers* (containers with tight cgroup
    memory limits, embedded targets) should set
    `OC_RSYNC_SPILL_THRESHOLD_BYTES` to a value below the cgroup
    limit (typical starting point: 64 MiB) and point
    `OC_RSYNC_SPILL_DIR` at a fast tmpfs or local SSD.
  - *Adversarial fan-out workloads* (deep INC_RECURSE trees with
    many unfinished segments held in the reorder window) benefit
    from `OC_RSYNC_SPILL_RECLAIM=re-spill` to keep the post-reload
    footprint bounded under sustained pressure.
  - *Slow or SMR spill directories* should set
    `OC_RSYNC_SPILL_COMPRESSION=zstd` to trade CPU for disk
    bandwidth; default level 3 is usually appropriate, raise to
    `zstd:7` only when the spill device is the bottleneck.
  - *Diagnostic granularity* on benchmark runs:
    `OC_RSYNC_SPILL_GRANULARITY=per-item` smooths the memory curve
    at the cost of more syscalls per spill event.
- **When to stay on the default.** Every workload that fits inside
  the documented 64 MiB high-water mark on the audit baseline. Spill
  is opt-in for a reason: the in-memory path is the fastest and
  byte-stable.
- **CLI flags.** `--spill-dir` and `--spill-threshold-bytes` are
  planned for STN-11 and will land in a future release; they shadow
  the two highest-value env vars. The remaining three knobs stay
  env-only.
- **References.** Public-API surface and validation rules:
  [`docs/design/spill-policy-public-api.md`](design/spill-policy-public-api.md).
  Spillable buffer internals:
  [`docs/design/reorderbuffer-spill-to-tempfile.md`](design/reorderbuffer-spill-to-tempfile.md).

### Combining flags

The feature flags are independent. A common production combination
for a high-concurrency Linux daemon endpoint is
`--features async-daemon,thread-slab-pool`. A common client-side
combination for high-RTT remote pulls is `--features async-ssh`.
Default builds remain tokio-free and ship every previous-release
behaviour unchanged.

## 5. CI matrix expansion

vNEXT expands CI to include cross-OS coverage for the new feature
flags plus dedicated macOS and Windows interop smoke harnesses. This
is infrastructure-only; operators do not need to do anything, but it
means users on macOS and Windows now see the same green-CI signal
that Linux users have always seen.

### New rows

The `feature-flags-cross-os` matrix runs four feature rows
(`async`, `tracing`, `serde`, `concurrent-sessions`) on
`ubuntu-latest`, `macos-latest`, and `windows-latest` (12 jobs).
Linux-only rows (`io_uring`, `copy_file_range`, crypto / deflate
backends) stay in the `feature-flags-linux` matrix.

### New interop jobs

- `interop (macOS)` runs `tools/ci/run_interop_smoke.sh` against
  Homebrew's current upstream rsync (>= 3.4.x). Scenarios: baseline
  upstream local copy, push, pull, quick-check no-op, delta both
  directions, `--list-only` parity. Required check.
- `interop (Windows, best-effort)` validates `oc-rsync.exe` against
  MSYS2/Cygwin upstream rsync for push, pull, and delta. Marked
  `continue-on-error` until baseline parity is green; promotes to
  required after that.

### macOS additions

The `macos-test` matrix now also runs the `metadata` and `apple-fs`
crates on every toolchain row, covering the Darwin `acl_exacl`
branch, the macOS timestamp path, and the AppleDouble + resource-fork
pipeline. Tests requiring root self-skip via `geteuid()`;
xattr-dependent tests probe support and skip on filesystems that lack
it.

### What this means for operators

- macOS and Windows binaries are exercised by interop tests on every
  PR. Regressions on those platforms are caught before release.
- Feature-flag combinations are exercised across all three host
  operating systems. A green release tag means the flag combinations
  in section 4 all built and tested cleanly on Linux, macOS, and
  Windows.

## 6. Performance characteristics changes

DDP and the new pool primitives change the shape of receiver-side
performance vs the previous release. Wall-clock totals are unchanged
to within noise on the common workloads (local copy, single-file
push/pull); the differences appear at the tails.

### Parallel delete planning vs serial emitter trade-off

Under the previous release, the entire deletion sweep ran in a single
batched pre-transfer phase, with per-directory scans fanning out on
rayon above 64 entries. The wall-clock cost of deletion was
front-loaded.

Under vNEXT:

- **Plan compute is still parallel.** Per-segment `compute_extras`
  jobs run on rayon as INC_RECURSE segments arrive. The CPU cost of
  scanning destination directories and intersecting with the filter
  chain is amortised across the transfer loop instead of front-loaded.
- **Emission is serial.** A single `DeleteEmitter` thread owns every
  unlink, every `*deleting` line, and every `DeleteStats` mutation.
  This guarantees byte-identical event order with upstream but caps
  the emission throughput at single-thread speed.
- **Net wall-clock impact.** For deletion-heavy workloads
  (`--delete-during` with thousands of extras per directory) the
  parallel compute typically completes before the emitter is
  bottlenecked on unlink syscalls, so the net is neutral to slightly
  positive. For deletion-light workloads the serial emitter is
  trivially fast.
- **Throughput sensitivity.** Filesystems where `unlink` is slow
  (NFS, FUSE, network filesystems) become the long pole earlier than
  before. If you previously relied on parallel batched unlinks to
  hide NFS latency, profile your workload; the deterministic single
  emitter cannot parallelise across that filesystem call. For such
  cases, consider running deletion as a pre-pass with
  `--delete-before` (still serial, but moved out of the transfer
  interleave) or running the transfer without `--delete` and
  separately reconciling.

### io_uring pool primitives

Two new pool primitives ship in `crates/fast_io/src/io_uring/`:

- **`SessionRingPool`** - bounded MPMC fleet
  (`min(available_parallelism(), 16)` slots) for daemon-session
  bursts. Amortises `io_uring_setup(2)` across many short-lived
  sessions.
- **`ThreadLocalRingPool`** - one ring per OS thread for pinned
  consumers (disk-commit, rayon workers). No locks on the submit/reap
  fast path.

These are additive primitives. Existing single-owner `SharedRing`
consumers (`disk_batch`, `file_writer`, `file_reader`) keep working
unchanged. Operators see no behavioural change; the wins are paid out
as consumers migrate to the new pools in subsequent releases.

### Buffer pool sharding

`thread-slab-pool` (section 4) shifts buffer-pool memory from a
single shared queue to a per-thread slab. Steady-state idle memory
grows by `N_threads * byte_cap` (default 1 MiB). Operators running
with more than 32 worker threads per pool will see lower contention
and slightly higher RSS.

## 6a. Windows NTFS ACL behaviour

`--acls`/`-A` now works on Windows targets via
`GetNamedSecurityInfoW`/`SetNamedSecurityInfoW`, but the implementation
is a Tier 1C partial path. Operators migrating Windows workloads should
budget for the documented lossy cases before flipping `-A` on:

- Explicit deny ACEs are dropped on send.
- Inherited ACEs are not transmitted; the destination inheritance chain
  takes over.
- The system ACL (SACL) is skipped unless the planned **--audit-acls**
  flag is passed and `SE_SECURITY_NAME` is held.
- Non-`rwx` access bits (`DELETE`, `WRITE_DAC`, `WRITE_OWNER`, generic
  bits) collapse to `r`/`w`/`x` plus `SYNCHRONIZE` on receive.
- Trustee SIDs that cannot be translated to or from an account name are
  dropped with a one-time warning.

The cross-platform payload remains byte-compatible with upstream rsync
and POSIX peers. The planned **--windows-acls** opt-in adds a higher-
fidelity SDDL payload over the existing xattr stream for Windows-to-
Windows transfers, and **--fail-on-windows-acl-loss** turns the lossy
cases into a hard failure (exit code 23) for environments that need to
preserve every NTFS ACE verbatim or abort. None of these three flags
ship in this release; track `docs/design/windows-ntfs-acl-support.md`
section 4 for the rollout schedule.

The full mapping matrix, hardlink-safe DACL application rules, and the
SDDL wire format details are in
`docs/design/windows-ntfs-acl-support.md`. The user-facing
**--acls** entry in `docs/oc-rsync.1.md` enumerates the lossy cases
alongside the flag synopsis.

## 7. Rollback procedure

If a regression surfaces in vNEXT, pin to the previous release. The
wire protocol is unchanged, so a partial rollback (some clients new,
some old; or client on one version, daemon on another) is safe.

### Pin a release via cargo

```sh
cargo install oc-rsync --version <PREVIOUS_VERSION> --locked
```

Replace `<PREVIOUS_VERSION>` with the last known-good tag (e.g.
`0.6.2`). The `--locked` flag pins transitive dependencies to the
release's `Cargo.lock`.

### Pin a release via the GitHub release page

Download the platform binary from
<https://github.com/oferchen/rsync/releases> for the previous tag.
Replace `/usr/local/bin/oc-rsync` (or your install path) with the
downloaded binary. The binary is statically linked on Linux musl
targets; on macOS and Windows the platform-native build is used.

### Pin a release via package manager

- **Homebrew (macOS):**
  `brew install oc-rsync@<PREVIOUS_VERSION>` if the formula
  publishes pinned versions; otherwise download the bottle from the
  release page.
- **Cargo workspace pin:** in a downstream workspace that depends on
  the oc-rsync crates, set `oc-rsync = "=<PREVIOUS_VERSION>"` in
  `Cargo.toml` and rerun `cargo update -p oc-rsync`.

### Behavioural rollback notes

- Reverting to the previous release restores the batched
  `--delete-during` sweep. The `--delete-strict-order` opt-in flag
  from the prior prerelease becomes available again.
- The opt-in feature flags from section 4 (`async-ssh`,
  `async-daemon`, `ssh-socketpair-stderr`,
  `thread-slab-pool`, `vmsplice`) do not exist in earlier releases.
  Builds that enabled them must drop the flag from the build command
  when downgrading.
- Wire compatibility is preserved across the rollback. A vNEXT
  client talking to a previous-version daemon (or the reverse) is a
  supported configuration; the protocol negotiation collapses to
  protocol 32 in both directions.

### Filing a regression report

If you trip a regression, capture:

- The exact invocation (sender side and receiver side).
- The output of `oc-rsync --version` on both ends.
- A `-vvv` log from a minimal reproducer.
- The transport (local copy, SSH subprocess, daemon TCP).
- Whether any of the opt-in feature flags were enabled in the build.

Open an issue at <https://github.com/oferchen/rsync/issues> with
those five pieces of information. Wire-level regressions are highest
priority; performance regressions on the workloads in section 6 are
next.

## Appendix: design and architecture references

| Topic                                          | Document                                                                |
|------------------------------------------------|-------------------------------------------------------------------------|
| Session architectural overview                 | `docs/architecture/session-overview-ddp-async-iouring.md`               |
| DDP specification                              | `docs/design/parallel-deterministic-delete.md`                          |
| Legacy strict-order gate (SUPERSEDED)          | `docs/design/delete-during-strict-order-gate.md`                        |
| Delete architecture                            | `docs/architecture/delete-during.md`                                    |
| SSH transport async I/O evaluation             | `docs/design/ssh-transport-async-io-eval.md`                            |
| Daemon async runtime choice                    | `docs/design/daemon-async-runtime-choice.md`                            |
| Daemon async accept + sync workers             | `docs/design/daemon-async-accept-sync-workers.md`                       |
| Parallel receive-side delta application        | `docs/design/parallel-receive-delta-application.md`                     |
| SSH stderr socketpair channel                  | `docs/design/socketpair-stderr-channel.md`                              |
| SSH stderr handling audit                      | `docs/audits/ssh-stderr-handling.md`                                    |
| Per-thread buffer slab                         | `docs/design/per-thread-buffer-slab.md`                                 |
| vmsplice / splice zero copy                    | `docs/design/splice-vmsplice-zero-copy.md`                              |
| io_uring session ring pool                     | `docs/design/iouring-session-ring-pool.md`                              |
| io_uring per-thread rings                      | `docs/design/iouring-per-thread-rings.md`                               |
| Cross-platform CI coverage                     | `docs/audits/cross-platform-ci-coverage.md`                             |
| Windows NTFS ACL support                       | `docs/design/windows-ntfs-acl-support.md`                               |
