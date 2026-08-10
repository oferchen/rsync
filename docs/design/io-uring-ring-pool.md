# io_uring Per-Session Ring Pool (#1936)

## Summary

`SharedRing` (`crates/fast_io/src/io_uring/shared_ring.rs:192`) lets a
single oc-rsync process amortise io_uring setup across reader and
writer paths within one transfer. The remaining footprint problem is
*cross-process*: when many oc-rsync invocations run concurrently on a
host (50+ jobs on a backup server is routine), each invocation
constructs its own ring(s). Every ring pins kernel pages for the
SQ/CQ regions plus the registered buffer set, multiplied per process.
On a backup host this is hundreds of MB of unevictable kernel memory.

This document scopes what we can realistically do without protocol
changes or true cross-process ring sharing, and lists concrete,
independently-shippable recommendations.

The in-process round-robin pool design is
[`docs/design/iouring-session-ring-pool.md`](./iouring-session-ring-pool.md);
the bgid namespace bound landed via #2044 (`BgidAllocator` in
`crates/fast_io/src/io_uring/buffer_ring.rs:275`), and the adaptive
buffer pool work is tracked separately as #2045.

## 1. Current State Survey

### 1.1 Ring construction sites

A single oc-rsync receiver invocation creates **at most one ring per
transfer**, gated behind `IoUringPolicy`. Construction is centralised:

- `crates/transfer/src/disk_commit/thread.rs:78` -
  `try_create_disk_batch()` builds an `IoUringConfig`, applies
  `--io-uring-depth` if set, and calls `IoUringDiskBatch::try_new` on
  `Auto` / `Enabled`. The batch is owned by the disk-commit thread for
  the lifetime of the transfer and re-used across files.
- `crates/transfer/src/pipeline/receiver.rs:79` -
  `PipelinedReceiver::new` spawns the disk thread (and therefore the
  ring) on receiver construction.
- `crates/transfer/src/receiver/transfer/pipeline.rs:138` -
  `PipelinedReceiver::new(disk_config)` is the single call-site in the
  receiver pipeline. The sender path does not construct a ring today.
- `crates/fast_io/src/io_uring/disk_batch.rs:70` -
  `IoUringDiskBatch::new` calls `IoUringConfig::build_ring()` which
  invokes `io_uring_setup(2)`.

`SharedRing::try_new`
(`crates/fast_io/src/io_uring/shared_ring.rs:220`) and the per-channel
`IoUringReader` / `IoUringWriter` factories
(`crates/fast_io/src/io_uring/file_factory.rs`) exist as primitives
but are **not wired into the production receiver path**; only the
batched disk writer is.

The in-process `RingPool` referenced in `iouring-session-ring-pool.md`
is the second piece, gated behind `IoUringConfig` and not yet wired
from the transfer pipeline.

### 1.2 Defaults

`IoUringConfig::default()` (`crates/fast_io/src/io_uring/config.rs:368`):

| Field                       | Default       |
|----------------------------|---------------|
| `sq_entries`               | 64            |
| `buffer_size`              | 64 KiB        |
| `register_files`           | true          |
| `sqpoll`                   | false         |
| `register_buffers`         | true          |
| `registered_buffer_count`  | 8             |

`MAX_REGISTERED_BUFFERS = 1024`
(`crates/fast_io/src/io_uring/registered_buffers.rs:80`). The
`for_large_files()` preset bumps these to 256 entries / 256 KiB / 16
buffers (`config.rs:386`); `for_small_files()` drops them to 128
entries / 16 KiB / 8 buffers (`config.rs:402`). The default is what
production code uses unless the operator passes `--io-uring-depth`.

CLI / wire knobs already in place:

- `--io-uring`, `--no-io-uring` set `IoUringPolicy` on both client and
  daemon (`crates/cli/src/frontend/server/flags.rs:128`).
- `--io-uring-depth=N` overrides `sq_entries` (validated by
  `validate_io_uring_depth`, `crates/fast_io/src/lib.rs:649`, bounded
  by `IO_URING_DEPTH_MAX = 32768` at `lib.rs:606`). It is forwarded to
  the daemon side (`crates/core/src/client/remote/invocation/builder.rs:165`).

### 1.3 Rings per invocation

One transfer = one ring (the disk-commit ring). The ring lives on the
dedicated disk commit thread and is dropped when the transfer
finishes. `--dry-run` and list-only modes bypass `PipelinedReceiver`
entirely (`crates/transfer/src/receiver/transfer/pipeline.rs:401`,
`run_dry_run_loop`), so no ring is constructed for those paths today.

### 1.4 Memory cost per ring

At defaults the per-ring kernel-pinned cost is approximately:

- **SQ ring**: 64 SQEs * 64 B = 4 KiB, rounded to one 4 KiB page.
- **CQ ring**: 128 CQEs * 16 B = 2 KiB (kernel sizes CQ at 2*SQ),
  also rounded to one page.
- **Registered buffers**: 8 buffers * 64 KiB, each page-aligned via
  `next_multiple_of(page_size)` at
  `crates/fast_io/src/io_uring/registered_buffers.rs:272`. Total = 512
  KiB pinned.
- Plus the io_uring instance fd and book-keeping.

Round figure: **~520 KiB pinned per ring** at defaults. 50 concurrent
oc-rsync invocations = ~26 MiB of pinned kernel pages just for ring
state, which is small in isolation but compounds with backup-time
metadata pressure (page cache eviction, OOM-killer scoring on
constrained boxes). On a host that runs `for_large_files()` presets
under load the per-ring figure climbs to 16 * 256 KiB = **4 MiB
pinned**, which is the regime where operators have observed RSS
pressure.

## 2. Per-Session Framing

### 2.1 What is a session?

For pooling purposes the working definition is **the receiver-side
transfer**: one entry to `PipelinedReceiver::new` corresponds to one
disk-commit thread, one ring, one file list. Daemon connections can
host multiple sessions back-to-back; the daemon parent process is not
a session, the per-connection transfer is. The CLI invocation owns
exactly one session for client-mode transfers.

This matches the SessionId framing in
`iouring-session-ring-pool-impl.md` (#1937): the pool is keyed by the
transfer identity, not by the OS process.

### 2.2 Within-process vs cross-process

- **Within-process pooling** (#1937 / `RingPool`) is tractable:
  rings are leased via a small fixed pool, reused across workers,
  released on session end. The design exists and is partly
  implemented.
- **Cross-process ring sharing** is **not** tractable without
  oc-rsync running its own long-lived daemon and brokering ring
  leases over IPC. io_uring fds can be passed via `SCM_RIGHTS`, but
  the SQ/CQ memory and registered buffers are not safely shareable
  across address spaces - the kernel's `io_uring_setup(2)` ties them
  to the constructing task's mm. SQPOLL with shared rings exists but
  requires per-task setup that defeats the goal.

The practical path for the cross-process case is therefore
**reducing per-process cost**, not sharing rings between processes.
The recommendations below are scoped to that.

## 3. Recommendations

Each is independently implementable and can ship as its own PR.

### R1. Lazy ring construction at the first SQE-bound call

**Today** `IoUringDiskBatch::try_new` is called unconditionally at
`disk_thread_main` start (`disk_commit/thread.rs:179`), even if the
transfer never writes a single byte (e.g. all files quick-checked out,
all destinations identical, errored handshake, generator-only roles).

**Change** `try_create_disk_batch` to return a builder/lazy handle
that defers `IoUringConfig::build_ring()` until the first
`begin_file()` call. The disk thread sees a `MaybeBatch` enum:

```text
MaybeBatch::Pending(IoUringConfig) -> on first begin_file, build_ring()
MaybeBatch::Active(IoUringDiskBatch)
MaybeBatch::Disabled
```

Concretely, move the `config.build_ring()?` from `disk_batch.rs:71`
into `IoUringDiskBatch::begin_file` (`disk_batch.rs:102`). The state
machine fits inside `IoUringDiskBatch` itself - the disk thread does
not need to change.

**Eliminates ring construction for**: `--dry-run` already bypasses, but
this also covers the "receiver started, no Begin message arrived
before an error / cancel" case, plus daemon connections that get as
far as starting the receiver but never receive data (auth-after-start
failure modes, EOF before first Begin).

**Risk**: tiny - the first-file latency picks up one
`io_uring_setup(2)` call (~50 us on a warm cache). Amortised across
the file's writes this is invisible.

### R2. Adaptive registered-buffer count

**Today** `registered_buffer_count = 8` is fixed regardless of
transfer size (`config.rs:377`). At 64 KiB each that is 512 KiB
pinned even for a 4 KiB transfer.

**Change** scale the count by a known-at-init transfer-size hint when
the file list is available. Buckets:

| Transfer size hint        | Buffers |
|--------------------------|---------|
| Unknown / not yet known   | 2       |
| < 1 MiB total             | 2       |
| < 64 MiB total            | 4       |
| < 1 GiB total             | 8       |
| >= 1 GiB total            | 16      |

The hint comes from `DiskCommitConfig.file_list` already plumbed
through (`crates/transfer/src/receiver/transfer/pipeline.rs:130`).
Sum the entries' sizes during `DiskCommitConfig` build and pass the
total as a new `io_uring_buffer_hint: u64` field consumed by
`IoUringDiskBatch::new`.

Upper bound stays at `MAX_REGISTERED_BUFFERS = 1024`
(`registered_buffers.rs:80`); the bgid namespace bound from #2044
(`BgidAllocator`, `buffer_ring.rs:275`) is the hard cap on parallel
buffer rings if multi-ring pooling lands later.

**Saves** at the small end: 384 KiB pinned per ring (8 -> 2 buffers).
50 concurrent jobs on small transfers = ~19 MiB saved.

**Risk**: under-sized pools for surprise-large transfers degrade to
the unregistered-buffer fallback path (already exercised; see
`shared_ring.rs:266` for the existing graceful-degradation pattern).
This is correctness-equivalent and only costs the per-SQE
`get_user_pages()` overhead.

### R3. Soft cap via env var / CLI flag

Add a process-wide soft cap on rings:

- CLI: `--io-uring-max-rings=N` (default unset = no cap).
- Env: `OC_RSYNC_IO_URING_MAX_RINGS=N` for setups where the CLI is
  managed by automation (cron, systemd unit, backup orchestrator) and
  the operator wants to enforce a fleet-wide policy without touching
  every invocation script.

When the cap is reached and another ring is requested, fall back to
standard I/O for that lease (which is already the silent fallback
behaviour for `try_create_disk_batch` returning `None`). The cap is
process-local; it cannot enforce a host-wide budget without IPC
coordination (out of scope).

This is most useful in two regimes:
1. **Memory-constrained hosts** (containers, embedded backup boxes).
2. **Concurrent-invocation test rigs** where a fixed budget keeps
   benchmark variance low.

Validation reuses `validate_io_uring_depth`'s
`IoUringDepthError`-style enum so the CLI parser stays uniform
(`lib.rs:649`).

**Risk**: low. The fallback path already exists and is tested.

### R4. Probe sharing (already in place - confirm and cite)

The `is_io_uring_available()` probe at
`crates/fast_io/src/io_uring/config.rs:179` already caches the result
in two process-wide atomics (`IO_URING_AVAILABLE`, `IO_URING_CHECKED`
at `config.rs:22-23`), and `pbuf_ring_supported()` caches its
`uname(2)` parse in a `OnceLock` (`buffer_ring.rs:324`). Every
subsequent call is a relaxed atomic load.

**No code change.** This recommendation exists to lock in the
invariant: future ring-related probes (e.g. `IORING_OP_SEND_ZC`
support) must use the same `OnceLock` / atomic-cache pattern, not a
per-call `uname(2)` or per-call `io_uring_setup(2)` probe. Reviewers
checking new io_uring code should reject probe call-sites that do not
cache.

## 4. Out of Scope

- **True cross-process ring sharing.** Requires oc-rsync to run an
  always-on daemon brokering ring leases over a Unix-domain socket or
  similar. That is a protocol change (new daemon mode, new IPC
  contract), and the gains do not justify the surface area at the
  current scale. Revisit if backup-fleet operators report >100 MiB
  pinned regressions.
- **SQPOLL changes.** SQPOLL is already opt-in via `IoUringConfig`
  (`config.rs:336`) and falls back transparently when
  `CAP_SYS_NICE` is missing (`config.rs:445`). Cross-process SQPOLL
  thread pinning is in `iouring-session-ring-pool.md` discussions and
  intentionally not covered here.
- **`SCM_RIGHTS` ring fd transfer.** Theoretically possible, but the
  kernel ties ring memory to the constructing mm; passing the fd does
  not let the receiving process submit on the ring. Out of scope.
- **In-process MPMC pool migration.** Tracked separately in
  `iouring-session-ring-pool.md` (#1937).

## 5. Migration Plan

Dependency graph:

```
R4 (no change, doc-only)        independent
R1 (lazy construction)          independent of R2, R3
R2 (adaptive buffer count)      independent of R1, R3
R3 (soft cap CLI/env)           independent of R1, R2
```

All four can land as parallel PRs. Suggested order by impact:

1. **R1** first - eliminates the most ring constructions at zero
   memory cost change for the active-transfer case. Lowest risk and
   gives operators a measurable RSS reduction on
   handshake-failure-heavy workloads.
2. **R2** second - delivers the biggest per-ring saving on the small-
   transfer side, which is where the 50+ concurrent invocations
   scenario lives.
3. **R3** third - operator-visible opt-in. Documentation-heavy. Needs
   coordination with the existing `--io-uring-depth` flag in
   `crates/cli/src/frontend/server/flags.rs:128`.
4. **R4** is a doc-only commit; it can ride with any of the above.

Each PR ships with:

- Unit tests on the new state machine / sizing function.
- A receiver pipeline integration test that asserts no ring is
  constructed on a `--dry-run` and on a transfer with zero files.
- A doc update referencing the new flag / env var in the relevant
  `docs/` page (R3 only).

The companion in-process work in #1937 can land independently and
does not block any of R1-R4.

## References

- `crates/fast_io/src/io_uring/shared_ring.rs:192` - `SharedRing`.
- `crates/fast_io/src/io_uring/shared_ring.rs:220` - `try_new`.
- `crates/fast_io/src/io_uring/config.rs:179` - cached
  `is_io_uring_available`.
- `crates/fast_io/src/io_uring/config.rs:368` - `IoUringConfig`
  defaults.
- `crates/fast_io/src/io_uring/disk_batch.rs:70` -
  `IoUringDiskBatch::new`.
- `crates/fast_io/src/io_uring/buffer_ring.rs:275` - `BgidAllocator`
  (#2044, merged via PR #4005).
- `crates/fast_io/src/io_uring/registered_buffers.rs:80` -
  `MAX_REGISTERED_BUFFERS`.
- `crates/transfer/src/disk_commit/thread.rs:78` -
  `try_create_disk_batch` call site.
- `crates/transfer/src/pipeline/receiver.rs:79` -
  `PipelinedReceiver::new` (session entry point).
- `crates/transfer/src/receiver/transfer/pipeline.rs:138` -
  receiver wiring of the disk thread.
- `crates/transfer/src/receiver/transfer/pipeline.rs:401` -
  `run_dry_run_loop` (no ring path).
- `crates/cli/src/frontend/server/flags.rs:128` - `--io-uring` and
  `--no-io-uring`.
- `crates/fast_io/src/lib.rs:649` - `validate_io_uring_depth`.
- `docs/design/iouring-session-ring-pool.md` - in-process pool
  (#1409 / #1937).
- `docs/design/iouring-session-ring-pool-impl.md` - implementation
  plan for #1937.
- `docs/design/io-uring-bgid-namespace.md` - bgid namespace bound
  (#2044).
- `docs/design/io-uring-adaptive-buffer-pool.md` /
  `docs/design/iouring-adaptive-buffer-pool.md` - adaptive sizing
  (#2045).
