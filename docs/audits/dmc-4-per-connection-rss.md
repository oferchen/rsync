# DMC-4: per-connection RSS audit for `--max-connections` default

Task: DMC-4. Companion work: DMC-1 (initial spec), DMC-3 (wire-byte pin),
DMC-5 (admission logging, pending #4831), DMC-6 (per-module override).
Related audit: `docs/audits/rss-3-fileentry-size-breakdown.md`
(per-`FileEntry` 88 B inline + heap, 2.8x-11x ratio vs upstream).

## Summary

The current `--max-connections` default is **`None` (unlimited)**, set at
`crates/daemon/src/daemon/runtime_options/types.rs:91` and gated by
`refuse_if_at_capacity()` at `crates/daemon/src/daemon/sections/server_runtime/connection.rs:120-154`.
When `None`, every accepted TCP socket spawns an OS thread
(`thread::spawn` at `connection.rs:178`) with no admission cap.

Combined with the flist RSS amplification documented in RSS-3 (per-entry
~128 B oc-rsync vs ~45 B upstream, **2.8x**) and `Vec<FileEntry>` slack
(see `rss-flist-vec-vs-pool.md`, **3-11x** peak), a daemon serving N
concurrent transfers multiplies the gap by N. On a memory-constrained
host (1-4 GiB) the unlimited default is a denial-of-service surface:
~30 concurrent 1 M-file transfers will exhaust 16 GiB of RSS alone.

### Recommendation

Keep the **default as `None` (unlimited)** for parity with upstream
rsync's `max connections = 0` global semantics, but **document the
sizing tradeoff** in `docs/DAEMON_PROCESS_MODEL.md` and in the
forthcoming `oc-rsyncd.conf(5)` manpage so operators size it
explicitly. Suggested wording is at the end of this audit.

Changing the compiled-in default would diverge from upstream behaviour
(upstream `lp_max_connections() == 0` means unlimited per module). The
operator-facing safer-by-default lever is a recommended sizing formula,
not a hardcoded ceiling.

## Per-connection RSS budget

The daemon allocates the following per accepted connection, all
charged to the per-thread `handle_session()` stack
(`crates/daemon/src/daemon/sections/session_runtime.rs:44`):

| Component | Source | Bytes | Notes |
|---|---|---|---|
| OS thread stack (resident) | `thread::spawn` default | ~16-64 KiB | 8 MiB virtual on glibc; resident grows on use. Empirically 16-32 KiB during handshake, 64-128 KiB mid-transfer. |
| `BufReader` over TcpStream | `session_runtime.rs:220` | 8 KiB | std default `BufReader::new` capacity. |
| `Arc<Vec<ModuleRuntime>>` clone | `connection.rs:169` | 0 (Arc) | Shared; no per-conn copy. |
| `Arc<Vec<String>>` MOTD | `connection.rs:170` | 0 (Arc) | Shared. |
| `Arc<SharedLogSink>` | `connection.rs:171` | 0 (Arc) | Shared. |
| `ConnGuard` counter slot | `connection.rs:172` | < 64 B | `AtomicUsize` increment + RAII handle. |
| File handles (per session) | accepted socket + work fds | ~32-128 B (kernel side) | Userspace tracker negligible; kernel `struct file` per fd not counted in RSS. |
| `DiskCommitConfig` SPSC channels (receiver only) | `crates/transfer/src/disk_commit/thread.rs:49-51`, capacity `DEFAULT_CHANNEL_CAPACITY = 128` (`config.rs:32`) | ~96 KiB | 128 `FileMessage` slots + 256 `io::Result<CommitResult>` slots + 256 `Vec<u8>` slots; slots are pre-allocated by `crossbeam_queue::ArrayQueue`. Each `FileMessage` is `enum { Begin(Box<BeginMessage>), Chunk(Vec<u8>), Commit, WholeFile{...}, ... }` -> tag + max variant payload ~64 B. Total: ~8 KiB tag/discriminant + ~88 KiB enum payloads. Charged only when the connection performs a receive. |
| Buffer pool: per-thread slab | `crates/engine/src/local_copy/buffer_pool/thread_slab.rs:58-61`, `DEFAULT_SLAB_SLOT_CAP=8`, `DEFAULT_SLAB_BYTE_CAP=8 * COPY_BUFFER_SIZE` (=8 MiB) | ~8 MiB peak | Allocated lazily on first I/O; cap is byte-budget, not slot-count. Returned to the global pool when slab drops with the worker thread. |
| io_uring per-ring (Linux + `io_uring` feature) | `crates/fast_io/src/io_uring_common.rs:124-139`, default `sq_entries=64`, `buffer_size=64 KiB`, `registered_buffer_count=8` | ~512 KiB | 64 SQE + 128 CQE descriptors (each ~64 B) ~12 KiB + 8 registered buffers * 64 KiB = 512 KiB. Allocated only when the receiver actually uses io_uring; the shared-ring path (post IUR-3 mitigation: per-thread rings) puts one ring per worker thread, so per-session. |
| flist (sender side) | `Vec<FileEntry>` + `PathBuf` heap | **see workload table below** | The dominant variable. |

Steady-state floor (no transfer in flight, just authenticated and
idle): **~24-72 KiB**. Almost entirely thread stack + 8 KiB
`BufReader`.

## Workload buckets

The numbers below are per-connection peak RSS, summing the components
above plus the workload-specific flist. They use the RSS-3 figure of
**~128 B inline + heap per entry** for a vanilla regular-file flist
(20 B basenames, 12 B parent dirs, 100 unique dirs).

### Bucket S: small / idle

- 10-100 entries, handshake-only, or `#list` reply.
- Flist: < 16 KiB (Vec doubling: 128 entries * 128 B = 16 KiB).
- Buffer pool slab: 0 (no I/O).
- Disk-commit channels: 0 (sender-only or list).
- io_uring: 0.

**Per-conn peak: ~32-96 KiB.**

### Bucket M: medium / 10 K-file transfer

- 10,000 entries, mixed regular files, no `-A -X -H --atimes`.
- Flist inline + heap: 10,000 * 128 B = **1.28 MiB** (oc-rsync) vs
  ~450 KiB (upstream). Vec doubling slack rounds Vec capacity to
  16,384, adding 88 B * 6,384 = 562 KiB inline slack.
  See `rss-flist-vec-vs-pool.md`.
- Buffer pool: ~8 MiB peak slab while transferring.
- Disk-commit channels (receiver): ~96 KiB.
- io_uring (Linux): ~512 KiB if active.

**Per-conn peak: ~10-12 MiB. Receiver-side bias.** Upstream rsync's
equivalent is ~3.5-4 MiB; the gap is dominated by buffer pool (which
oc-rsync provisions more aggressively for throughput) and Vec slack.

### Bucket L: large / 1 M-file transfer

- 1,000,000 entries, vanilla flist (no extras).
- Flist inline + heap: 1,000,000 * 128 B = **128 MiB** (oc-rsync) vs
  ~45 MiB (upstream). Vec slack rounds to 1,048,576 capacity (clean
  power of 2, ~0 KiB slack); the 83 MiB gap is per-entry overhead.
- With `-A -X -H --atimes --crtimes --checksum`: extras box ~256 B
  rounded plus optional heap. **Per-entry ~352-400 B**, **352-400 MiB
  flist**. Upstream pays ~64-80 MiB (pool-packed extras).
- Buffer pool: ~8 MiB.
- Disk-commit channels: ~96 KiB.
- io_uring: ~512 KiB.

**Per-conn peak: ~140 MiB (vanilla) or ~370-410 MiB (full-extras).**
Upstream equivalent: ~55-90 MiB.

## Cross-tab: total daemon RSS by `N` and workload

Shared daemon overhead (modules, log sink, config, MOTD): ~2-8 MiB
regardless of N. Below are total daemon-process RSS estimates summing
shared overhead + N * per-conn-peak. The right column shows the
maximum N that fits each common host RAM bucket if **every** session
is simultaneously at peak.

| Workload | per-conn peak | Fits 1 GiB | Fits 4 GiB | Fits 16 GiB | Fits 64 GiB |
|---|---|---|---|---|---|
| Bucket S (handshake / list) | 64 KiB | ~15,000 | ~62,000 | ~250,000 | ~1,000,000 |
| Bucket M (10 K-file xfer) | 12 MiB | ~80 | ~340 | ~1,360 | ~5,500 |
| Bucket L (1 M-file vanilla) | 140 MiB | ~7 | ~28 | ~115 | ~460 |
| Bucket L (1 M-file extras) | 400 MiB | ~2 | ~10 | ~40 | ~160 |

Important caveats:

- Real workloads mix buckets. The accept loop refuses on the
  *cumulative* in-flight thread count, not weighted by size, so the
  worst-case dimensioning above assumes adversarial concurrent peaks.
- The buffer pool slab is reclaimed when the worker thread exits, so
  RSS is reuse-friendly across sequential sessions on the same
  thread; the daemon currently spawns a *fresh* thread per accept,
  so the slab is paid per session, not per thread lifetime.
- Linux glibc `malloc` does not return arena pages to the kernel
  eagerly; even after a session ends, RSS may stay elevated until
  arena trimming or `malloc_trim()`. Realistic steady-state RSS for
  a long-running daemon will trend toward the **high-watermark**
  reached during peak concurrency, not the current concurrency.

## Comparison to upstream

Upstream rsync's `max connections` directive is per-module, defaults
to `0` (unlimited), and is enforced via `clientserver.c:744-756`
`claim_connection()`. Upstream forks per connection, so the
per-process kernel accounting (`ulimit -v`, cgroup memory limits) is
the natural backstop. oc-rsync's threaded model collapses all sessions
into a single kernel-accounted process: there is no `ulimit` per
session, only the shared process limit. This makes the
`--max-connections` cap **more load-bearing** for oc-rsync than for
upstream, because there is no kernel-enforced safety net once a single
session goes pathological.

Wire compatibility: the `@ERROR: max connections (N) reached -- try
again later` message is pinned byte-for-byte by DMC-3 (test
`daemon_max_connections_wire_bytes_match_upstream`).

## Recommendation

1. **Leave the default at `None` (unlimited)** to preserve parity with
   upstream `max connections = 0` semantics. Operators who run a
   daemon on a memory-constrained host already need to think about
   filtering, chroot, and bandwidth; `--max-connections` is one knob
   in that toolbox. A surprise non-zero default would break interop
   expectations and the wire-compat narrative.
2. **Document the sizing formula** so the unlimited default is an
   informed choice rather than an unconsidered one. Suggested
   wording for `docs/DAEMON_PROCESS_MODEL.md` under "Operational
   Recommendations":

   > **Memory-aware sizing for `--max-connections`.** Per-connection
   > peak RSS depends on workload. For a typical mixed workload,
   > budget **~12 MiB per concurrent transfer** (10 K-file vanilla
   > flist) or **~140-400 MiB per concurrent transfer** for very
   > large flists (1 M files, with or without `-A -X -H --atimes`).
   > A safe starting point on a host with `M` GiB of free RAM is
   > `--max-connections = floor(M * 1024 / 16)` for general-purpose
   > workloads, or `--max-connections = floor(M * 1024 / 150)` for
   > known-large flists. The default (`None`) leaves admission
   > unlimited and matches upstream rsync's `max connections = 0`;
   > do not run a publicly exposed unlimited daemon on a host with
   > less than 4 GiB of free RAM. The per-module `max connections`
   > directive overrides the global flag on a per-module basis
   > (see DMC-6).
3. **File a follow-up to track the gap shrinking.** RSS-4 and RSS-5
   are scoped to bring the per-entry overhead toward upstream parity.
   Once those land, the per-connection budget shrinks and the safe-N
   formula loosens. Re-run this audit after RSS-5 ships and revise
   the recommended sizing constants.

## Cross-references

- `crates/daemon/src/daemon/runtime_options/types.rs:17,91` -
  `max_connections: Option<NonZeroUsize>` default `None`.
- `crates/daemon/src/daemon/runtime_options/parsing.rs:74-78` -
  CLI `--max-connections` parser.
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:120-154` -
  `refuse_if_at_capacity()` admission check.
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:163-227` -
  `spawn_connection_worker()` per-connection thread.
- `crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs` -
  shared `ConnectionCounter` (`AtomicUsize`).
- `crates/daemon/src/daemon/module_state/connection_limiter.rs:28-44` -
  per-module file-based limiter for cross-process counts.
- `crates/transfer/src/disk_commit/config.rs:32-127` -
  `DEFAULT_CHANNEL_CAPACITY = 128`, channel capacity clamp range
  `[8, 4096]`.
- `crates/transfer/src/pipeline/messages.rs:21-46` - `FileMessage`
  enum (channel payload).
- `crates/transfer/src/disk_commit/thread.rs:47-58` -
  `spawn_disk_thread()` and 3 SPSC channels per receive session.
- `crates/engine/src/local_copy/buffer_pool/thread_slab.rs:58-61` -
  per-thread buffer slab caps (8 slots / 8 MiB).
- `crates/fast_io/src/io_uring_common.rs:124-174` - `IoUringConfig`
  default (`sq_entries=64`, 8 * 64 KiB registered buffers).
- `docs/audits/rss-3-fileentry-size-breakdown.md` - per-entry
  88 B + ~32 B heap accounting.
- `docs/audits/rss-flist-vec-vs-pool.md` - Vec doubling slack.
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md` - empirical
  100 K-file RSS measurements (42.6 MB oc-rsync vs 7.9 MB upstream).
- `docs/DAEMON_PROCESS_MODEL.md:139-194` - current operator-facing
  doc for `--max-connections`; the proposed wording above extends
  section "Operational Recommendations".
- `target/interop/upstream-src/rsync-3.4.1/clientserver.c:744-756` -
  upstream `claim_connection()` reference.
