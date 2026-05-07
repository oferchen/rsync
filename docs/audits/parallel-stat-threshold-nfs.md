# Parallel stat threshold tuning for NFS and FUSE workloads

Task: #1084. Branch: `docs/parallel-stat-threshold-nfs-1084`.

## Summary

The transfer crate switches from sequential to rayon-parallel `stat()`/
`lstat()` once a per-call list crosses a fixed threshold of 64 entries. The
constant was tuned for local ext4/APFS/NTFS, where a cold metadata lookup
costs single-digit microseconds. On NFSv3, NFSv4, SMB-via-FUSE, sshfs, and
other high-latency filesystems each `getattr` is a network round-trip, the
attribute cache invalidates aggressively, and the server happily services
hundreds of inflight RPCs per session. Under those conditions a higher
threshold (256, 1024, or "always parallel") materially shortens phase B of
the receiver's quick-check and the generator's batch stat. This audit lists
the present constants, summarises NFS/FUSE stat semantics, sketches why
larger thresholds win on remote filesystems, and proposes a detection +
override strategy that keeps local workloads on the current curve.

## Current threshold

The single source of truth is `crates/transfer/src/parallel_io.rs:13-33`:

- `DEFAULT_STAT_THRESHOLD: usize = 64` (line 16) gates parallel `stat()`.
- `DEFAULT_SIGNATURE_THRESHOLD: usize = 32` (line 22) gates parallel
  signature builds.
- `DEFAULT_METADATA_THRESHOLD: usize = 64` (line 27) gates parallel
  `chmod`/`chown`/`utimes` application.
- `DEFAULT_DELETION_THRESHOLD: usize = 64` (line 33) gates parallel
  `read_dir` scans during `--delete`.

`ParallelThresholds` (lines 44-95) wraps the four values into a
`Copy` struct with builder setters, and the consumers thread it through:

- `crates/transfer/src/generator/file_list/batch_stat.rs:43` calls
  `map_blocking(paths, thresholds.stat, ...)` for the generator's batch
  stat phase.
- `crates/transfer/src/receiver/transfer/candidates.rs:124-132` runs phase
  B of the receiver quick-check at `self.parallel_thresholds.stat`.
- `crates/transfer/src/receiver/directory/creation.rs:122-124` applies
  per-directory metadata at `self.parallel_thresholds.metadata`.
- `crates/transfer/src/receiver/directory/deletion.rs:99-101` walks
  delete candidates at `self.parallel_thresholds.deletion`.
- `crates/transfer/src/receiver/transfer/pipeline.rs:179-186` reads
  `self.parallel_thresholds.signature` for parallel signature work.

`map_blocking` (lines 107-125) implements the gate: below the threshold it
collects with `Iterator::map`, above it dispatches via
`into_par_iter().map().collect()`. The doc comment at line 4 records the
rationale: "For lists below `min_parallel`, falls back to sequential
`Iterator::map` to avoid thread-pool dispatch overhead." The 64-entry
choice predates any NFS/FUSE measurement and has never been revisited in
this repository. Property tests in the same file (lines 259-318) only
verify ordering and sequential/parallel parity; they do not bound the
constant.

## NFS and FUSE stat semantics

### NFS

Linux NFS issues one `GETATTR` (NFSv3) or one `OP_GETATTR` inside a
COMPOUND (NFSv4) per `stat()`/`lstat()` syscall whenever the attribute
cache for the inode is cold or expired. The cache window is governed by
the `acregmin`/`acregmax` (regular files) and `acdirmin`/`acdirmax`
(directories) mount options, defaulting to 3-60 s and 30-60 s
respectively. Inside a single rsync run, every freshly-discovered path is
a cold lookup: the file list arrives from the sender, the receiver has
never touched those inodes locally, and there is no cached `fattr`.

Per-call cost on a typical LAN NFS server is 0.3-1.0 ms (RTT plus
server-side disk latency), three to four orders of magnitude above the
sub-microsecond cost on local ext4. The Linux NFS client maintains a
slot table, defaulting to 65536 active RPCs per TCP connection (see
`/sys/module/sunrpc/parameters/max_session_slots` and the `nconnect`
mount option for parallel TCP streams). The `nfsd` thread pool on the
server, set with `RPCNFSDCOUNT`, defaults to 8 threads on Debian/Ubuntu
and 16 on RHEL, so a single client can keep 8-16 RPCs in flight before
queuing on the server side.

Practical implication: with 64 paths the receiver issues 64 sequential
`stat()` syscalls, each blocking the caller for one RTT. End-to-end that
is 19-64 ms wall time on a 0.3-1.0 ms link. Splitting the same 64 paths
across rayon's pool drops the wall time toward `64 / parallelism * RTT`,
limited by server thread count.

### FUSE

FUSE follows a request/response pattern over `/dev/fuse`. Every
`stat()` becomes a `FUSE_GETATTR` request handed to the userspace
filesystem daemon. Two FUSE settings control batching:

- `entry_timeout` and `attr_timeout` (returned per-entry by the daemon)
  control how long the kernel may cache `lookup` and `getattr` results.
  Many cloud-backed daemons (rclone, s3fs, gcsfuse) set these to 0 to
  preserve consistency, forcing a userspace round-trip on every call.
- `FUSE_CAP_PARALLEL_DIROPS` (kernel >= 5.1) lets the kernel issue
  multiple lookup requests on the same directory concurrently. Without
  this flag the kernel serialises directory operations even when the
  caller submits them from multiple threads.

For sshfs the per-call cost mirrors NFS: one SFTP packet per `stat`,
one RTT plus a small server-side fstat. For object-store FUSE
implementations the cost is dominated by HTTP HEAD round-trips
(20-200 ms each), so even modest parallelism wins large absolute
amounts of wall time. The per-call latency floor and the daemon's
inflight-request limit (rclone defaults to `--vfs-cache-max-size`
controlled, s3fs to `multireq_max=20`) are the relevant constraints,
not CPU cost.

### Server-side concurrency ceilings

A higher client-side parallelism only pays off up to the server's
inflight ceiling:

| Filesystem | Typical server concurrency | Notes |
|------------|---------------------------|-------|
| Linux nfsd | 8-16 threads | tunable via `RPCNFSDCOUNT` |
| Ganesha NFS | 64 worker threads | `Nb_Worker` config |
| sshfs | 1-2 (sftp-server) | one channel per connection |
| s3fs | 5-20 inflight HTTP | `multireq_max` mount option |
| rclone mount | tunable, typically 4-16 | `--transfers`, `--checkers` |
| SMB via cifs.ko | 8 credits default | server-issued `MaxMpxCount` |

Saturating these limits with rayon's default thread count (one per CPU)
yields near-optimal throughput. Going above the ceiling produces queueing
on the server with no further client speedup but no measurable harm,
since rayon dispatches into a fixed-size pool.

## Why a higher threshold helps slow filesystems

Today on NFS, with 100 paths and a 1 ms RTT:

- Sequential below threshold: 64 paths cross the gate and parallelise,
  but the threshold is dominated by the gate test, not by the work cost.
  A 50-path call stays sequential and pays 50 ms.
- 200 paths: parallelised, 200 / 8 cores * 1 ms = 25 ms vs 200 ms
  sequential, an 8x speedup.

A larger threshold is not what slow filesystems need - the present 64
already favours parallelism. The actionable change is the opposite:
*lower* the threshold (or set it to 0/`usize::MIN`) on detected NFS/FUSE
mounts so even small batches dispatch through the pool. The proposal
below frames it as "higher than the local default" because the relevant
local CPU-overhead threshold (where rayon dispatch overhead matches a
local stat) sits around 64; for NFS the equivalent crossover is closer
to 1 because every saved RTT dwarfs the dispatch cost.

Where a *higher* threshold may help is on a fast local filesystem
shadowed by a slow remote target via `--remote-source` style operations:
parallelism on the local stat side is already free at 64, but on the
remote side it can compound with the server-side connection pool,
swamping `nconnect=N`. Capping rayon at 256 or 1024 inflight matches the
typical NFS slot-table window without overcommitting.

The two distinct regimes argue for two distinct knobs rather than one
shared constant:

1. `min_parallel_remote` - small (1-8) for NFS/FUSE/SSH-mounted paths.
2. `max_inflight_remote` - rayon pool cap (or chunk size) honouring
   server slot tables and SSH window credits.

## Proposed detection and override strategy

### 1. Compile-time defaults remain

Keep `DEFAULT_STAT_THRESHOLD = 64` as the conservative local fallback.
Introduce `REMOTE_FS_STAT_THRESHOLD = 1` (or 4 to absorb syscall jitter)
as a sibling constant in `parallel_io.rs`. Both stay `pub const` so
embedders can override at compile time via cfg/feature flags.

### 2. Filesystem detection via statfs f_type (Linux)

On Linux, `statfs(2)` populates `f_type` with the magic number defined
in `<linux/magic.h>`. The relevant values are well-known and stable:

| Magic | Constant | Filesystem |
|-------|----------|------------|
| 0x6969 | `NFS_SUPER_MAGIC` | NFSv2/v3/v4 |
| 0x65735546 | `FUSE_SUPER_MAGIC` | any FUSE |
| 0xFF534D42 | `CIFS_MAGIC_NUMBER` | SMB/CIFS |
| 0x73757245 | `CODA_SUPER_MAGIC` | Coda (rare) |
| 0xfe534d42 | `SMB2_SUPER_MAGIC` | SMB2/3 |
| 0x517B | `SMB_SUPER_MAGIC` | legacy smbfs |

`rustix::fs::statfs` returns the magic in a portable wrapper. The
detection runs once per top-level destination directory, caches the
result on `ReceiverContext`, and selects either `DEFAULT_STAT_THRESHOLD`
or `REMOTE_FS_STAT_THRESHOLD` for that transfer. Per-file detection is
not needed - rsync transfers operate inside one filesystem tree per
destination root.

On macOS the equivalent is `fstatfs`/`getfsstat` populating
`f_fstypename` with strings such as `"nfs"`, `"smbfs"`, `"webdav"`,
`"osxfuse"`, `"macfuse"`. On Windows the equivalent is
`GetVolumeInformationW` (`FileSystemNameBuffer == "NFS"` or
detection of redirector drives via `GetDriveTypeW == DRIVE_REMOTE`).
Each platform path is gated behind `#[cfg(...)]` and falls back to the
local default when the call fails or the platform is unknown.

### 3. Per-filesystem environment override

A single environment variable composes cleanly with the existing
`ParallelThresholds` builder:

```
OC_RSYNC_STAT_THRESHOLD=256          # global override
OC_RSYNC_STAT_THRESHOLD_REMOTE=1     # only for detected NFS/FUSE/CIFS
OC_RSYNC_STAT_THRESHOLD_LOCAL=128    # only for detected local FSes
```

Parsing happens once at session start in `core::session()`, the result
flows into `CoreConfig::parallel_thresholds`, and the existing builder
setters propagate it to receiver/generator. Invalid values (negative,
non-numeric) fall back to defaults with a warning logged at debug level.
This mirrors the precedent set by `OC_RSYNC_BUFFER_POOL_SIZE` and
`OC_RSYNC_IO_URING_POLICY`.

### 4. CLI flag for explicit override

For deterministic benchmarking and operator control:

```
oc-rsync --parallel-stat-threshold=256 ...
oc-rsync --parallel-stat-mode=auto|always|never ...
```

`--parallel-stat-mode=auto` (default) uses statfs detection.
`always` forces `threshold = 1`. `never` forces `threshold = usize::MAX`.
The numeric override wins when both are specified, matching upstream
rsync's "last flag wins" convention. The flag stays hidden from the
short `--help` output and appears only in `--help-extended`, since this
is a tuning knob, not a user-facing feature.

### 5. Compile-time vs runtime trade-off

Compile-time only (cargo features) is too rigid: NFS detection happens
on the deployed host, not at build time, and operators tuning around a
specific server's `RPCNFSDCOUNT` need a runtime knob. Runtime-only
(env var + CLI) is sufficient on its own. Recommendation: ship the
const defaults, the env var, and the CLI flag; skip cargo features.

## Open questions

- Should rayon use a *separate* thread pool sized to NFS slot-table
  width when remote is detected, instead of the shared default pool?
  The current shared pool is sized to CPU count, which under-uses NFS
  parallelism on small servers and over-commits CPU when local work
  runs in parallel with stat I/O.
- For mixed transfers (local source + NFS destination), the threshold
  on the receiver should follow the destination filesystem, but the
  generator's batch stat on the source side should follow the source.
  Today both share `parallel_thresholds.stat`. Splitting into
  `stat_source` and `stat_dest` is a natural follow-on.
- Interaction with `--io-timeout`: a parallel batch with hundreds of
  inflight stats raises the worst-case completion time when one server
  goes slow. The timeout currently applies per-syscall via the kernel
  RPC layer, not at batch granularity, so this is benign, but worth
  noting for documentation.

## References

- `crates/transfer/src/parallel_io.rs`
- `crates/transfer/src/generator/file_list/batch_stat.rs`
- `crates/transfer/src/receiver/transfer/candidates.rs`
- `crates/transfer/src/receiver/directory/creation.rs`
- `crates/transfer/src/receiver/directory/deletion.rs`
- Linux `<linux/magic.h>` super-block magic numbers.
- RFC 1813 (NFSv3 GETATTR), RFC 7530 (NFSv4 OP_GETATTR).
- Linux kernel Documentation/filesystems/fuse.rst
  (FUSE_CAP_PARALLEL_DIROPS, attr_timeout semantics).
