# oc-rsync Extension Environment Reference

This page documents the oc-rsync-specific environment variables that tune
local behaviour of a production build. They are extensions, not upstream
rsync options:

- **Local only.** No variable on this page is ever forwarded to a remote
  peer. Each one affects only the process it is set for.
- **Observable-neutral.** These knobs change memory footprint, syscall
  profile, or timing - never wire bytes, transferred contents, stats,
  exit codes, or output.
- **Env-only by design.** Stable user-facing behaviour is controlled by CLI
  flags (sometimes with an env equivalent). Tuning and experimental knobs
  are deliberately env-only so the flag namespace stays close to upstream
  rsync; see `docs/design/cli-tunability-flags.md` section 8.
- **Safe to ignore.** Every variable has a tested default. Invalid values
  never abort a transfer: they are warned about (where a warning channel
  exists) and the default applies.

Test-only, benchmark-only, and CI-only variables (`OC_RSYNC_BENCH_*`,
`OC_RSYNC_BIN*`, `OC_RSYNC_UPSTREAM*`, `*_TEST`, `*_STRESS`, and similar)
are intentionally not documented here.

Unless noted otherwise, each variable is read once per process and cached;
changing it after startup has no effect on a running process.

## Buffer pool

The process-wide I/O buffer pool is shared by the receiver, generator, disk
commit, and parallel checksum paths (`crates/engine/src/local_copy/buffer_pool/`).

### OC_RSYNC_BUFFER_POOL_SIZE

Maximum number of buffers the global pool retains. Positive integer.
Default: the detected hardware parallelism (one buffer per hardware
thread). Zero, negative, or non-numeric values are ignored.
Intentionally env-only: the default covers the common case and the knob is
rarely tuned (`docs/design/cli-tunability-flags.md` section 8).

### OC_RSYNC_BYTE_BUDGET

Soft cap, in bytes, on memory retained by the pool. Returns past the
budget deallocate the buffer instead of pooling it; acquires never block.
Plain byte integer; `0` disables the budget (unbounded retention).
Default: 33554432 (32 MiB). An explicit `--max-alloc` value replaces this
budget. Caution: disabling the budget lets adaptive buffer sizing retain
memory without bound on large transfers.

### OC_RSYNC_BUFFER_POOL_MEMORY_CAP

Hard cap, in bytes, on outstanding (checked-out) buffer memory. When the
cap would be exceeded, `acquire` blocks until a buffer is returned
(backpressure). Accepts a positive byte count or the literal `auto`
(one quarter of detected physical RAM; uncapped if RAM cannot be
detected). `0`, unset, or invalid: uncapped (the default). Caution:
enabling the cap trades never-stalling acquires for a memory ceiling -
use it only when bounding peak RSS matters more than acquire latency.

### OC_BUFFER_POOL_BLOCK_SIZE

Per-buffer block size. Note the prefix: this variable is named
`OC_BUFFER_POOL_BLOCK_SIZE`, without `RSYNC_`. Accepts a size spec with
the usual rsync suffixes (`128K`, `4M`, `8388608`), parsed by the same
size parser as the CLI size options. Default: 131072 (128 KiB, the copy
buffer size). Values above 1 GiB are clamped with a warning; zero,
negative, or malformed values fall back to the default. Purely a local
I/O knob - it never affects the wire protocol or the delta block size.

### OC_RSYNC_BUFFER_POOL_STATS

Diagnostic dump. When set to exactly `1`, prints a one-line buffer-pool
telemetry summary (reuses, allocations, growths, byte overflows, hit
rate) to stderr when a pool is dropped. Default: off.

## Concurrency

### OC_RSYNC_ADAPTIVE_QUEUE

Escape hatch for the adaptive work-queue depth controller in the parallel
receive-delta pipeline. Adaptation is the default; setting a disabling
value (`0`, `false`, `off`, `no` - case-insensitive) pins the
deterministic static queue depth with no controller, for reproducible
runs and debugging. Any other value (or unset) keeps adaptation on.

### OC_RSYNC_DISK_COMMIT_CHANNEL_CAP

Capacity, in messages, of the SPSC channel between the receiver's network
thread and the disk-commit thread. Unsigned integer, clamped to 8..=4096.
Default: 128 (roughly 4 MiB of buffered chunks at the average chunk
size). Unset or unparseable values fall back to the default. Read at
disk-commit config construction. Purely a memory/pipelining trade-off:
larger values absorb burstier disk latency at the cost of peak RSS.

### OC_RSYNC_PIPELINE_WINDOW

Number of concurrent file requests the pipelined receiver keeps in
flight. Unsigned integer, clamped to 1..=256 (`1` is synchronous
operation). Default: 64. Unset or unparseable values fall back to the
default. Read at pipeline config construction. Larger windows hide more
per-file round-trip latency on high-latency links; each pending request
costs roughly 500 bytes.

### OC_RSYNC_REORDER_RING_CAP

Pins the per-file reorder-ring capacity used by the parallel delta
applier. Positive integer; no upper clamp by design. Default: 64.
`0` or unparseable values emit a one-shot stderr warning and fall back to
the default. Caution: very large values multiply per-file memory.

### OC_RSYNC_DASHMAP_SHARDS

Pins the shard count of the parallel delta applier's concurrent map,
bypassing the worker-count heuristic
(`(workers * 4).next_power_of_two()`, clamped to 4..=1024). Positive
integer, clamped to 4..=1024 and rounded up to the next power of two.
Invalid values fall back to the heuristic. Useful for micro-benchmarks
and hosts where sequential file indices cluster into few shards.

### OC_RSYNC_PARALLEL_CHECKSUM

Controls the parallel windowed basis-checksum generator on the receiving
side. Default: on (parallel). Setting `0`, `false`, `no`, or `off`
forces the sequential generator. An explicit `--checksum-threads` policy
takes precedence over this variable. Output is byte-identical either way.

## I/O backends

### OC_RSYNC_DISABLE_IOURING

Forces io_uring availability to report false, so all I/O uses the
standard buffered paths. Truthy values: `1`, `true`, `yes`, `on`
(case-insensitive). Equivalent in effect to the `--no-io-uring` flag;
the two are independent switches and either one alone disables io_uring -
when both are set they agree, and neither can re-enable what the other
disabled. Linux only (io_uring does not exist elsewhere); requires the
default `io_uring` build feature for the variable to matter at all.

### OC_RSYNC_MMAP_TO_SQPOLL_THRESHOLD_BYTES

Size threshold, in bytes, above which basis-file reads prefer
io_uring+SQPOLL over mmap. Base-10 unsigned integer. Default: 65536
(64 KiB). Malformed or empty values fall back to the default. Only
meaningful on Linux hosts where the io_uring basis-read path is active.

### OC_RSYNC_ADAPTIVE_BASIS_DISPATCH

Opt-out for the experimental throughput-adaptive basis-read dispatcher.
Requires the `adaptive-basis-dispatch` Cargo feature, which is off by
default - on a default build this variable is inert. When the feature is
compiled in, setting `0`, `off`, `false`, or `no` disables the adaptive
path at runtime and reverts to the static size-threshold rule above.

### OC_RSYNC_WIN_CHUNK_BYTES

Windows only. Chunk size for the chunked basis-file reader that caps
peak RSS at chunk size instead of file size. Must be a power of two in
4096..=67108864 (4 KiB to 64 MiB). Default: 4194304 (4 MiB). Invalid
values warn (via tracing) and fall back to the default.

### OC_RSYNC_WINDOWS_RIO

Windows only. Selects the Registered I/O (RIO) socket path for the TCP
daemon: `off` (default - standard IOCP socket path), `auto` (attempt
RIO, fall back to IOCP when unavailable), or `on` (require RIO).
Case-insensitive; unknown values mean `off`. Experimental.

### OC_RSYNC_BWLIMIT_BACKEND

Sleep primitive used by the `--bwlimit` pacing loop. Values
(case-insensitive): `std` / `thread` for `std::thread::sleep`, `kqueue` /
`timer` for the macOS kqueue `EVFILT_TIMER` sleeper (sub-millisecond
resolution). Default: kqueue on macOS, std elsewhere. Unknown values
fall back to the platform default; the kqueue request is ignored on
non-macOS hosts.

### OC_RSYNC_FORCE_ROOTLESS_CONTAINER

Forces the rootless-container detection to report a rootless verdict, so
SQPOLL is not attempted. Truthy values: `1`, `true`, `yes`, `on`
(case-insensitive). Default: real detection via `/proc` markers. Linux
only. Primarily an integration-test hook, but usable to pre-empt SQPOLL
setup in environments the heuristics miss.

## Spill (reorder-buffer overflow to disk)

These variables mirror the `--spill-dir`, `--spill-threshold-bytes`, and
`--no-spill` CLI flags. Precedence: CLI overrides are applied after the
env overrides, so a flag always wins over its variable. Spilling is
disabled by default (no threshold configured).

### OC_RSYNC_SPILL_DIR

Directory for spill files. Any non-empty path; validated when the spill
backend is constructed, not when the variable is parsed. Equivalent to
`--spill-dir`.

### OC_RSYNC_SPILL_THRESHOLD_BYTES

Byte threshold above which reorder-buffer contents spill to disk.
Unsigned integer. Unset: spilling stays disabled. Equivalent to
`--spill-threshold-bytes`.

### OC_RSYNC_NO_SPILL

In-memory-only mode: truthy values (`1`, `true`, `yes` -
case-insensitive) prevent the reorder buffer from writing to disk.
Equivalent to `--no-spill`.

### OC_RSYNC_SPILL_COMPRESSION

Compression for spilled payloads: `none`, `zstd`, or `zstd:N` (integer
level). Default: `none`. Any `zstd*` value requires the
`spill-compression` Cargo feature (off by default) and is rejected with a
warning otherwise.

## Daemon

### OC_RSYNC_CONFIG

Overrides the daemon configuration file path. Resolution order: a
`--config` argument wins, then `OC_RSYNC_CONFIG`, then the legacy
`RSYNCD_CONFIG`, then the brand-default candidate paths.

### OC_RSYNC_SECRETS

Overrides the daemon secrets file path. Same resolution order as
`OC_RSYNC_CONFIG`, with `RSYNCD_SECRETS` as the legacy name.

### OC_RSYNC_DAEMON_ADDRESS_FAMILY

Forces the listener address family: `ipv4` (aliases `v4`, `4`, `inet`),
`ipv6` (aliases `v6`, `6`, `inet6`), or `both` (aliases `dual`,
`dualstack`, `dual-stack`). Case-insensitive; unknown values are ignored
so a typo degrades to the compile-time default instead of failing
startup. Read once at accept-loop entry.

### OC_RSYNC_NO_SECCOMP

Runtime opt-out for the daemon worker seccomp filter. Requires the
`daemon-seccomp` Cargo feature (off by default); when compiled in, the
filter is on by default and any truthy value (not empty, `0`, or
`false`) disables it. Use when a workload trips a syscall missing from
the allowlist.

### OC_RSYNC_DAEMON_SECCOMP

Alias with inverse polarity for the same filter: `0` or `false` disables
it, matching the variable's historical opt-in spelling. Same
`daemon-seccomp` feature requirement as `OC_RSYNC_NO_SECCOMP`.

### OC_RSYNC_ASYNC_DAEMON

Opt-in async accept loop for the TCP daemon. Requires the `async-daemon`
Cargo feature (off by default); when compiled in, setting the variable to
any value selects the async path. Exists to enable async-vs-sync
concurrency comparisons; the sync accept loop is the production default.

Follow-up recommendation: the daemon knobs in this section may be better
served as `oc-rsyncd.conf` directives, which propagate cleanly to daemon
workers; flagged for a future design note, not implemented.

## Miscellaneous

### OC_RSYNC_ASYNC_SSH

Opt-in async SSH byte transport for client transfers. Requires the
`async-ssh` Cargo feature (off by default); when compiled in, truthy
values (`1`, `true`, `yes`, `on`) route SSH transfers through the tokio
transport. Default: the synchronous spawned-process path, matching
upstream behaviour.

### OC_RSYNC_BRAND

Overrides the detected brand identity (program names, config and secrets
path candidates). Accepts `oc` or `upstream`, case-insensitive, plus the
corresponding program-name aliases (`oc-rsync`, `rsync`, `rsyncd`).
Unset or unrecognised values keep the identity derived from the invoked
executable name. Intended for testing and development.

## Legacy variables

`OC_RSYNC_FALLBACK` and `OC_RSYNC_DAEMON_FALLBACK` are historical names
that are no longer consulted by any production code path; setting them
has no effect on current builds.
