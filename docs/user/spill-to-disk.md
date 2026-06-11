# Spill-to-disk

This guide describes the spill-to-disk feature, which provides a disk-backed
overflow valve for the reorder buffer used during parallel delta application.
When the in-memory reorder buffer exceeds a configured byte threshold, excess
items are serialized to a temporary file and reloaded transparently on demand.

Upstream rsync has no equivalent - it processes files sequentially in
`recv_files()` and never buffers more than one file's data at a time. The
spill mechanism handles the memory pressure that arises from parallel dispatch
reordering in oc-rsync.

## When it activates

Spilling is **off by default**. The default `SpillPolicy` sets
`threshold_bytes: None`, which selects the bare in-memory reorder buffer with
zero disk I/O. No temporary files are created and no filesystem interaction
occurs unless you explicitly opt in.

The spill layer engages only when you set one of:

- `--spill-threshold-bytes <N>` on the command line
- `OC_RSYNC_SPILL_THRESHOLD_BYTES=<N>` in the environment

Once a threshold is configured, the buffer monitors its estimated in-memory
footprint after each insert. When the estimate exceeds the threshold, items
furthest from the delivery cursor are serialized to disk until memory usage
drops back below the limit.

## Configuration

### Byte threshold

| Method | Example |
|--------|---------|
| CLI flag | `--spill-threshold-bytes 256M` |
| Env var | `OC_RSYNC_SPILL_THRESHOLD_BYTES=268435456` |

The CLI flag accepts a positive integer with an optional case-insensitive
suffix: K, M, G, T, P, E (base 1024). The environment variable accepts a
plain integer only. Zero is rejected - omit the flag to disable spilling.

Precedence: **CLI > env var > defaults** (off).

### Spill directory

| Method | Example |
|--------|---------|
| CLI flag | `--spill-dir /var/tmp/oc-rsync-spill` |
| Env var | `OC_RSYNC_SPILL_DIR=/var/tmp/oc-rsync-spill` |

Overrides the directory where the temporary spill file is created. When
unset, the buffer uses a `SpooledTempFile` backend that keeps small spills
in memory (up to 1 MB) before rolling over to a system tempfile in the OS
default temp directory. When set, an anonymous tempfile is created directly
inside the specified path.

Precedence: **CLI > env var > defaults** (system temp).

### Compression

| Method | Example |
|--------|---------|
| Env var | `OC_RSYNC_SPILL_COMPRESSION=zstd` |
| Env var (explicit level) | `OC_RSYNC_SPILL_COMPRESSION=zstd:3` |
| Env var (disable) | `OC_RSYNC_SPILL_COMPRESSION=none` |

Optional zstd compression on spilled payloads. Trades CPU time for reduced
disk write volume. Requires that oc-rsync was built with the
`spill-compression` Cargo feature - without it, `zstd` values are rejected
and the buffer uses uncompressed writes. Set to `none` (the default) for raw
bytes.

There is no CLI flag for compression - it is controlled exclusively through
the environment variable.

## Filesystem requirements

- **Writable directory.** The spill directory (or the system temp directory)
  must be writable by the oc-rsync process. The directory is created via
  `create_dir_all` if it does not exist when a spill is first attempted.

- **Sufficient free space.** The spill file grows as items are evicted from
  memory. Plan for headroom proportional to the transfer size minus the
  configured byte threshold. For example, a 10 GB transfer with a 256 MB
  threshold could spill up to several hundred megabytes, depending on how
  out-of-order chunks arrive.

- **Anonymous temp files.** The spill layer uses `tempfile::tempfile_in()`
  for directory-backed spills, which creates an anonymous (unlinked) file.
  If oc-rsync crashes, exits unexpectedly, or is killed, the OS reclaims the
  disk space automatically - no orphaned temp files are left behind.

## Failure behavior

The spill layer is designed for graceful degradation:

- **Write failure (ENOSPC, permission denied, etc.):** Items that fail to
  spill are re-inserted into the in-memory buffer. The transfer continues
  with those items resident in memory. The error is logged but does not
  immediately abort the transfer.

- **Directory vanishes mid-transfer (no prior spills on disk):** The buffer
  attempts a single `create_dir_all` recovery, resets its write cursor, and
  retries. If the retry succeeds, the transfer continues without data loss.

- **Directory vanishes mid-transfer (prior spills on disk):** Records already
  written to the vanished directory are unrecoverable. The buffer surfaces a
  `PriorSpillsLost` error with the directory path and the count of lost
  records. The receiver maps this to rsync exit code 11 (file I/O error) and
  aborts the transfer.

- **Unsupported compression tag on read:** If a spill file written by a build
  with `spill-compression` enabled is read by a build without the feature,
  the buffer reports `UnsupportedCompression` and aborts. This cannot happen
  during a single transfer - it would require swapping the binary mid-run.

In all cases, **no data corruption occurs.** Spill failures either keep items
in memory (graceful) or abort the transfer cleanly with an actionable error.

## When to use

Most users should leave the default (in-memory only). Consider enabling the
spill layer when:

- **Large transfers with many concurrent files** produce a high volume of
  out-of-order chunks that accumulate in the reorder buffer. The spill layer
  caps resident memory by offloading the furthest-from-delivery items to
  disk.

- **Memory-constrained systems** (containers, VMs, embedded) where the
  reorder buffer's memory footprint could cause OOM pressure. Setting a byte
  threshold prevents the buffer from growing unboundedly.

- **Long-running daemon transfers** where transient spikes in chunk arrival
  order could accumulate memory over hours. The spill layer sheds excess
  memory to disk and reclaims it on demand.

Do **not** enable spilling when:

- The transfer is small or the file count is low. The reorder buffer stays
  compact and the spill overhead (serialization, disk I/O, deserialization)
  provides no benefit.

- The temp directory is on slow storage (spinning disk, NFS, network mount).
  Spill writes on slow media can become the transfer bottleneck rather than
  the relief valve.

## Performance notes

- **Spilling adds disk I/O.** Each spill event writes serialized items to
  disk; each reload reads them back. On fast NVMe storage the overhead is
  negligible. On spinning disks or network mounts, spill I/O can dominate
  transfer time.

- **Compression trades CPU for disk bandwidth.** With `OC_RSYNC_SPILL_COMPRESSION=zstd`,
  payloads are zstd-compressed before writing and decompressed on reload.
  This reduces disk write volume at the cost of CPU cycles for the codec.
  Useful when disk bandwidth is the constraint; counterproductive when CPU
  is the bottleneck.

- **Hot zone prevents thrashing.** Items close to the delivery cursor
  (the "hot zone") are kept in memory even when the threshold is exceeded.
  This avoids the pathological case of spilling an item to disk only to
  reload it on the next delivery.

- **Spill strategy: furthest-first.** The buffer evicts items with the
  highest sequence numbers first - those are furthest from delivery and
  least likely to be needed soon. This maximizes the time before a reload
  is required.

- **Batch vs per-item granularity.** The default (`WholeBatch`) packs all
  eviction candidates into a single disk record, amortizing the per-record
  header overhead. The alternative (`PerItem`) writes one record per item
  for finer eviction control at higher per-record overhead. Both modes are
  internal to the implementation and not configurable via CLI.

## RSS-aware spilling

An additional trigger is available for Linux systems: the
`memory_pressure_bytes` policy knob forces a spill when process RSS crosses
a configured threshold, independent of the byte-budget accounting. This
catches cases where the reorder buffer's internal size estimate diverges from
actual process memory usage.

RSS-aware spilling is not currently exposed as a CLI flag or environment
variable - it is available only to callers that construct a `SpillPolicy`
programmatically. The RSS probe reads `/proc/self/statm` on Linux and caches
the result for 100 ms to avoid syscall overhead on the hot path. On macOS
the probe is stubbed (returns zero), and on Windows it returns an error -
both fall back to the byte-budget knob silently.

## Expected spill rate per workload class

Operators who enable spilling and see the one-shot spill warning (the
ROB-3 "spill-to-disk activated" log line) need to know whether activation
is expected for their workload or whether it points at a configuration
problem. The table below sets the expectation per workload class. Numbers
are spill activations (transitions from in-memory to disk) per transfer
when spilling is enabled - not per file and not per chunk.

| Workload | Expected spill (per transfer) | Notes |
|----------|-------------------------------|-------|
| Single small file | 0 | Well within the in-memory ring. Spill should never activate. |
| 100 to 1K-file local transfer | 0 | Well within the ring. Spill should never activate. |
| 100K-file local transfer, no parallel-receive-delta | 0 to 1 | Sequential path. Adversarial chunk ordering can produce a single spill burst; sustained spilling is not expected. |
| 100K-file transfer with parallel-receive-delta | 0 to N | Depends on worker count and chunk arrival ordering. Each adversarial reorder window can trigger one activation. N scales with worker count, not file count. |
| 1M-file transfer with INC_RECURSE | 0 to N | Natural pressure source. Multi-segment flists arriving while earlier segments are mid-transfer produce reorder pressure proportional to segment overlap. |
| Adversarial workload (testsuite, fuzz, fault injection) | Many | Expected test condition. The reorder-buffer property tests deliberately exercise spill paths. |

Rules of thumb:

- **Expected 0**: if your workload is in this row and you see a spill warning,
  this is a bug or a misconfiguration. File a report.
- **Expected 0 to 1**: a single activation per transfer is acceptable. If
  `spill_events` (see counters below) climbs above 1 for the transfer, treat
  it like the "many" row.
- **Expected 0 to N**: track `spill_events` over the transfer. If activations
  increase faster than worker count or flist segment count, the byte
  threshold is likely too low for the workload's reorder window.
- **Many**: only adversarial workloads should land here. Production transfers
  should not.

The ring-cap formulas that drive this matrix are documented in the
ROB-1 audit (`docs/audits/rob-1-reorder-ring-cap-audit.md`). The
adaptive-ring rollout that will narrow the "0 to N" rows over time is
specified in ROB-7 (`docs/design/rob-7-adaptive-reorder-ring.md`).

## What to do if you see the spill warning

The one-shot warning ("spill-to-disk activated") fires once per
`SpillableReorderBuffer` instance on the first transition from in-memory to
disk. It is informational, not an error - the transfer continues and
completes correctly. Use this checklist to decide whether action is needed:

1. **Is this workload in the "expected 0" class?** If yes, do not tune knobs.
   File a bug with the transfer's command line, file count, worker count, and
   the value of `spill_events` from the counters table. The warning indicates
   a missed sizing assumption in the buffer or an unexpected reorder pattern,
   not a runtime fault.

2. **Is this workload in the "expected 0 to 1 / 0 to N" class?** Check the
   `spill_events` counter (see "Diagnostic counters" below). If the counter
   reads `1` for the transfer, the workload's reorder window briefly exceeded
   the threshold once and spill did its job - no action needed. If the
   counter climbs above 1 per worker, tune the knobs in step 3.

3. **Is the workload in the "many" / adversarial class, or did step 2 produce
   sustained activations?** Tune the configuration:

   - Raise `--spill-threshold-bytes` / `OC_RSYNC_SPILL_THRESHOLD_BYTES` to
     widen the in-memory budget before disk engages. Doubling the threshold
     is a reasonable first step.
   - Override `OC_RSYNC_REORDER_RING_CAP` (added in ROB-11) to grow the ring
     itself, which delays threshold pressure. Use this when the reorder
     window is wide (deep parallel pipelines, dense delta on a single large
     file).
   - Point `--spill-dir` / `OC_RSYNC_SPILL_DIR` at fast local storage
     (NVMe, ramdisk). The default tempfile location is fine for occasional
     spills but becomes the transfer bottleneck under sustained activation.
   - For memory-constrained hosts that cannot raise the threshold, accept
     the spill activations and confirm via the counters that `reload_events`
     stays low (frequent reloads mean the hot-zone heuristic is losing,
     not the threshold).

4. **Confirm spill ran cleanly.** Check `spilled_items` is 0 at end of
   transfer (all evictions reloaded successfully) and that the exit code is 0
   or matches the upstream-rsync expectation for the operation. Spill never
   silently changes wire output - a spill warning with a clean exit is by
   design.

## Cross-references

- ROB-2 - spill-activations counter API (`SpillStats::spill_events`,
  surfaced via `DeltaConsumerStats`).
- ROB-3 - one-shot spill warning (the log line this section documents).
- ROB-7 - adaptive reorder-ring sizing design that targets the "0 to N"
  rows in the matrix above (`docs/design/rob-7-adaptive-reorder-ring.md`).
- ROB-13 - CI bench cell that asserts spill activations stay at zero for
  the "expected 0" workload classes.
- ROB-1 - ring-cap formula audit that the workload-class matrix derives
  from (`docs/audits/rob-1-reorder-ring-cap-audit.md`).

## Diagnostic counters

When spilling is active, the buffer tracks the following counters (available
via `SpillStats` in the engine API):

| Counter | Description |
|---------|-------------|
| `spilled_items` | Number of items currently on disk |
| `spill_events` | Total spill-to-disk events since buffer creation |
| `reload_events` | Total reload-from-disk events since buffer creation |
| `memory_used` | Current estimated in-memory bytes |
| `threshold` | Configured spill threshold in bytes |
| `dir_recreate_events` | Times the spill directory was re-created after vanishing |

## Examples

Spill to disk when the reorder buffer exceeds 256 MB:

```sh
oc-rsync -a --spill-threshold-bytes 256M source/ dest/
```

Same via environment variable:

```sh
export OC_RSYNC_SPILL_THRESHOLD_BYTES=268435456
oc-rsync -a source/ dest/
```

Spill to a specific directory with zstd compression:

```sh
export OC_RSYNC_SPILL_COMPRESSION=zstd:3
oc-rsync -a --spill-threshold-bytes 128M --spill-dir /fast-nvme/spill source/ dest/
```

Disable spilling (the default - no flags needed):

```sh
oc-rsync -a source/ dest/
```
