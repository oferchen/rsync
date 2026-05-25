# High-IOPS io_uring bench cell design (IUB-3)

Tracking: IUB-3 (#2856). Implementation lands under IUB-4 (#2857) and
IUB-5 (#2858). Predecessor inventory: IUB-1 (#2854 / PR #4879) at
`docs/audit/iouring-bench-workload-inventory.md`. Sibling design
(throughput-bound cells): IUB-2 (#2855 / PR #4883) at
`docs/design/iouring-multi-gb-bench-design.md`.

## 1. Scope

IUB-3 specifies the high-IOPS bench cells - workloads where the
bottleneck is the per-file syscall rate (open / statx / read / close /
rename / unlinkat), not raw byte throughput. The companion IUB-2 design
covers throughput-bound workloads (2 GiB / 10 GiB / 50 GiB single-file
cells on NVMe). The two designs produce distinct payoff matrices because
io_uring pays for itself differently in each regime:

- Throughput-bound (IUB-2): queue depth + large-block batching reduce
  wall time per MB transferred. Speedup scales with payload size.
- IOPS-bound (this doc): submission-queue batching amortises the
  per-syscall kernel cost across many small ops. Speedup scales with
  file count and tree-walk fanout.

The headline release bench at 148.3 MB across 10 000 files shows ~1.00x
io_uring vs stdlib (memory note
`project_iouring_marginal_at_small_bench_scale`). That cell is in the
crossover zone - too few files for IOPS dominance, too few bytes for
throughput dominance. IUB-3 cells push file count by 10x and 100x to
move firmly into the IOPS-bound regime.

Out of scope here: SQPOLL sweep (handled by
`iouring_sqpoll_vs_regular.rs`), SEND_ZC network path (handled by IUS-3
at `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`), and the
multi-GB single-file shape (handled by IUB-2).

## 2. Workload fixtures

Two scales, both deliberately small per-file so the per-op cost
dominates the per-byte cost:

### 2.1 100K-file workload

- File count: 100 000
- Per-file size: 4 KiB (one filesystem page)
- Total payload: ~400 MiB
- Per-file syscall sequence (sender side): `openat` + `statx` +
  `read` (single page) + `close`. Receiver adds `openat(O_CREAT)` +
  `write` + `close` + `rename` + `fsync`.
- Expected per-file syscall count (sender): 4 entries + 1 dir-walk
  `getdents64` amortised. At 100K files that is ~400 K data-path
  syscalls plus tree-walk - the regime where `io_uring_enter` batching
  pays off if it is going to pay off at all.

### 2.2 1M-file workload

- File count: 1 000 000
- Per-file size: 1 KiB (sub-page; many filesystems still issue a full
  block read)
- Total payload: ~1 GiB
- Targets containerised / packaged-rootfs use cases: distroless
  images, OCI layers, package manager caches (`/var/lib/pacman`,
  `/var/lib/apt`, `node_modules`). These workloads are realistic for
  oc-rsync's container-image sync use case.
- Stresses the upper IOPS envelope. At 1M files, even small fixed
  per-op overheads dominate wall time.

Both fixtures are deterministic - the same seed produces the same
bytes - so checksum-mode runs are repeatable and so cross-host
comparisons are valid.

## 3. Fixture generation

A single generator script drives both scales:
`scripts/iouring_high_iops_fixture.py` (added under IUB-4).

### 3.1 Path layout

Tree depth 3 with fan-out tuned so that directory enumeration dominates
flat iteration (mirrors real-world deep trees like `node_modules` and
package caches):

```
fixture-root/
  d000/ ... d031/                        (32 dirs at depth 1)
    d000/ ... d031/                      (32 dirs at depth 2)
      d000/ ... d031/                    (32 dirs at depth 3)
        f00000.bin ... f00097.bin        (~98 files per leaf dir)
```

Counts:

- 100K cell: 32 x 32 x 32 = 32 768 leaf dirs x ~3 files per leaf =
  ~98 304 files. Adjust last leaf to land on exactly 100 000.
- 1M cell: same shape, ~31 files per leaf = ~1 015 808 leaves. Adjust
  to exactly 1 000 000.

Rationale for depth-3 fanout: getdents64 cost per directory is roughly
constant under a few dozen entries; the wall-time bottleneck at this
scale is the openat/close per file, not the dir scan itself. Depth-3
keeps inode locality high (each dir's children share a parent inode)
while still forcing the tree-walker to enumerate ~32 K directories at
the 100K scale, defeating any flat-iteration shortcut.

### 3.2 File content

Deterministic seed-derived pseudo-random bytes:

```python
seed = blake2b(f"oc-rsync-iub3-{scale}-{file_index}".encode()).digest()
content = chacha20(key=seed[:32], nonce=seed[32:44]).keystream(size_bytes)
```

ChaCha20 (or equivalent stream cipher) over a per-file-derived key
produces incompressible bytes, which prevents the rsync compression
codec from collapsing the working set. The Python `cryptography`
package provides ChaCha20; otherwise the generator can shell out to
`openssl enc -chacha20`.

### 3.3 Idempotency

Generator writes a sentinel file at `fixture-root/.iub3-sentinel`
containing:

```
scale=<100k|1M>
seed=<blake2b hex of seed corpus>
file_count=<exact count>
generated_at=<unix ts>
```

On invocation:

1. If sentinel exists and `scale` + `seed` + `file_count` match the
   args, exit 0 with `"reusing existing fixture"`.
2. If sentinel exists but mismatches, refuse to overwrite and exit
   non-zero with a message pointing at `--force`.
3. If sentinel absent, generate, then write sentinel last (so a
   crash mid-generation leaves no sentinel and the next invocation
   regenerates).

### 3.4 CLI

```
iouring_high_iops_fixture.py --scale {100k,1M} --root <path> [--force]
                             [--seed <hex>] [--jobs N]
```

- `--scale 100k` or `--scale 1M` selects the cell.
- `--root` is the fixture parent dir (the script appends `iub3-100k`
  or `iub3-1M`).
- `--force` overrides the sentinel mismatch refusal.
- `--seed` overrides the default seed (only for debugging
  reproducibility regressions).
- `--jobs N` parallelises file creation across N worker threads;
  defaults to `min(os.cpu_count(), 16)`. At 1M files, generation
  itself takes 5-15 min on NVMe even parallelised, which is why
  idempotency matters.

## 4. Metrics

Per cell, capture:

1. **Wall time** - hyperfine; 10 measured runs after 1 warmup.
   `hyperfine -w 1 -r 10 --export-json ...`.
2. **Syscalls/sec** -
   `perf stat -e syscalls:sys_enter_openat,syscalls:sys_enter_statx,syscalls:sys_enter_read,syscalls:sys_enter_close,syscalls:sys_enter_io_uring_enter`
   wrapping a single bench run. Compute rate as `count / wall_time`.
   Out-of-band: do not run perf inside the timed hyperfine loop -
   perf's ptrace overhead skews wall time. Run hyperfine once for
   timing, perf once for syscall counts, both unattended.
3. **Page faults** - `perf stat -e faults,minor-faults,major-faults`
   alongside the syscall capture. Cold-start cells should show high
   major-faults (basis reads from disk); warm-cache cells should
   show near-zero major-faults.
4. **oc-rsync internal counters** - if `fast_io` already exposes
   per-syscall counters (`open`, `statx`, `rename`, `unlinkat`),
   wire them up via `--debug=stats` or an equivalent dump. If the
   counters are not exposed today, **flag the gap in the IUB-4
   results doc** and rely on perf `syscalls:sys_enter_*` instead. Do
   not block the bench on counter plumbing - perf gives us the same
   data from outside the process.
5. **Peak RSS** - `/usr/bin/time -v` (or `gtime -v` on macOS).
   Capture from a single non-timed run wrapping the binary directly,
   not via hyperfine (hyperfine forks). The `Maximum resident set
   size` line is the metric. Cross-link
   `project_rss_3_11x_upstream` - we know oc-rsync currently runs
   3-11x upstream RSS at flist scale, so this metric is expected to
   fail the 1.5x gate at the 1M-file cell and we should document
   that as a known gap rather than as an IUB-3 blocker.

All metrics land in `target/iub-3/{scale}/{variant}/{cell}.json` plus
a flat summary CSV at `target/iub-3/summary.csv` so the IUB-10 payoff
matrix can ingest both IUB-2 and IUB-3 results from one place.

## 5. Cell matrix

```
{ scale }              x  { binary }                       x  { cache }
  100K-file                oc-rsync io_uring=on                cold
  1M-file                  oc-rsync io_uring=off               warm
                           upstream rsync 3.4.1
```

= 2 x 3 x 2 = **12 cells**.

| cell-id | scale | binary | cache state |
|---------|-------|--------|-------------|
| `iub3/100k/iouring_on/cold` | 100 K | oc-rsync io_uring=on | cold |
| `iub3/100k/iouring_on/warm` | 100 K | oc-rsync io_uring=on | warm |
| `iub3/100k/iouring_off/cold` | 100 K | oc-rsync io_uring=off | cold |
| `iub3/100k/iouring_off/warm` | 100 K | oc-rsync io_uring=off | warm |
| `iub3/100k/upstream/cold` | 100 K | upstream 3.4.1 | cold |
| `iub3/100k/upstream/warm` | 100 K | upstream 3.4.1 | warm |
| `iub3/1M/iouring_on/cold` | 1 M | oc-rsync io_uring=on | cold |
| `iub3/1M/iouring_on/warm` | 1 M | oc-rsync io_uring=on | warm |
| `iub3/1M/iouring_off/cold` | 1 M | oc-rsync io_uring=off | cold |
| `iub3/1M/iouring_off/warm` | 1 M | oc-rsync io_uring=off | warm |
| `iub3/1M/upstream/cold` | 1 M | upstream 3.4.1 | cold |
| `iub3/1M/upstream/warm` | 1 M | upstream 3.4.1 | warm |

### 5.1 Cache-state mechanics

- **Cold-start.** Before each timed run, execute
  `sync && echo 3 | sudo tee /proc/sys/vm/drop_caches`. The bench
  harness (IUB-4 / IUB-5) MUST surface a clear error if not run as
  root or with `sudo` cached - never silently degrade to warm-cache.
  An optional fallback is per-file
  `posix_fadvise(POSIX_FADV_DONTNEED)` looped over every fixture
  file; this works rootless but is slow at 1M files (5-10 min just
  to drop the cache). Default to `drop_caches`, document the
  fallback.
- **Warm-cache.** Run one untimed warm-up (cache-priming) pass, then
  10 timed runs back-to-back. Hyperfine's `--warmup 1` covers this.
  At the 1M-file scale, warming the cache itself takes ~30 s on
  NVMe; budget accordingly.

### 5.2 Binary selection

- `oc-rsync io_uring=on`: built with `--features io_uring`, run with
  `OC_RSYNC_IO_URING=force` (or whatever the IUB-4 dispatch flag
  resolves to). Both reads and writes should dispatch through the
  ring.
- `oc-rsync io_uring=off`: same binary, run with
  `OC_RSYNC_IO_URING=off` so dispatch falls back to stdlib. This
  isolates the io_uring backend from any other 0.6.x improvements
  vs upstream.
- `upstream rsync 3.4.1`: the system package or the binary from
  `target/interop/upstream-src/rsync-3.4.1/rsync`. Always run with
  the same protocol-level flags as the oc-rsync runs (`-a` minimum)
  so the comparison is apples-to-apples.

## 6. Pass / Fail criteria

For IUB-7 (100K) and IUB-9 (1M) result review, the following gates
apply per cell:

### 6.1 Regression gate: io_uring on vs io_uring off

`wall(iouring_on) <= wall(iouring_off) * 1.00` in both cold and warm
runs, both scales. **io_uring on MUST NOT be slower than io_uring
off** - if it is, the io_uring backend is paying queue-management
costs without recovering them on small-file I/O, and we should add a
file-count guard to `fast_io::dispatch` that routes high-IOPS work
to stdlib. Cite memory note
`project_iouring_marginal_at_small_bench_scale` in any IUB-7 / IUB-9
report that trips this gate.

### 6.2 Parity gate: oc-rsync vs upstream

`wall(oc-rsync best of {on, off}) <= wall(upstream) * 1.05` in both
cold and warm runs, both scales. **Either io_uring mode must land
within 1.05x of upstream wall time.** This is the headline
"oc-rsync is at least as fast as upstream" gate. Misses here are
release blockers for the next minor version.

### 6.3 RSS gate: peak memory

`rss(oc-rsync best of {on, off}) <= rss(upstream) * 1.5` in both
runs, both scales. **Peak RSS must be within 1.5x of upstream.** We
know this gate likely fails today at the 1M-file cell - cross-link
`project_rss_3_11x_upstream`. IUB-7 / IUB-9 should report the actual
ratio rather than masking the failure; a > 1.5x ratio at 1M files
becomes a tracked issue under the RSS reduction effort, not a
blocker for the IUB-3 result publication.

### 6.4 Syscall-rate confirmation

For the io_uring-on cells, `syscalls:sys_enter_io_uring_enter` should
be on the order of `(file_count / batch_size)` and `sys_enter_openat`
+ `sys_enter_read` + `sys_enter_close` should be strictly lower than
the io_uring-off cells. If io_uring-on shows higher total syscalls
than io_uring-off, the bench is mis-configured (likely fixed-buffer
registration is not active) and the result must be discarded and
rerun before any of the gates above are applied.

## 7. Cross-references

These memory notes apply to IUB-3 and should be cited inline in the
IUB-7 / IUB-9 result docs:

- `[[project_iouring_marginal_at_small_bench_scale]]` - the 148 MB /
  10 K-file baseline that motivates IUB-3. If the 100K cell still
  shows ~1.00x, escalate; if it shows > 1.10x, we have confirmed the
  file-count threshold where io_uring starts paying off.
- `[[project_iouring_kernel_version_floor]]` - basic ring is Linux
  5.1, full perf tier needs 6.0+. Document the kernel version in
  every IUB-7 / IUB-9 result row; RHEL 8 (4.18) cannot run these
  cells at all and must be flagged as N/A, not as a failure.
- `[[project_rss_3_11x_upstream]]` - feeds directly into Section 6.3.
  Expected failure at 1M-file cell; reference the note in the
  failure justification.

Two more notes that may surface during IUB-7 / IUB-9:

- `[[project_io_uring_shared_ring_bottleneck]]` - if the io_uring-on
  cells flatline at high file counts despite the kernel floor being
  satisfied, the single shared `Arc<Mutex<Ring>>` is the likely
  cause. Cross-link the note in the result doc.
- `[[project_iouring_send_zc_optin_only]]` - irrelevant to the IUB-3
  disk path but mention if anyone asks why the cells do not exercise
  the wire path.

## 8. Implementation pointer

For IUB-4 (harness wiring) and IUB-5 (cell bodies):

- **Bench file location.** `crates/fast_io/benches/iouring_high_iops.rs`,
  mirroring the existing `iouring_per_file_vs_shared.rs` and
  `iouring_sqpoll_vs_regular.rs` layout. Same crate, same feature
  gates, same env-gate convention.
- **Harness choice.** **Criterion**, not hyperfine, for the in-tree
  cells - matches every other io_uring bench in the repo (see IUB-1
  inventory; all 31 in-tree cells are criterion) and lets us share
  `scripts/benchmark_io_optimizations.sh` plumbing. Hyperfine appears
  in this design only for the out-of-band wall-time / RSS capture
  driven from `xtask` (Section 4); the criterion cells handle the
  in-tree statistical sampling.
- **xtask wrapper.** Add `xtask bench iouring-high-iops --scale
  {100k,1M} --variant {iouring-on,iouring-off,upstream} --cache
  {cold,warm}` that:
  1. Ensures the fixture exists (calls
     `scripts/iouring_high_iops_fixture.py --scale ...`).
  2. Drops caches if `--cache cold`.
  3. Invokes the chosen binary via hyperfine, captures wall time.
  4. Re-invokes once under perf for syscall + page-fault counts.
  5. Re-invokes once under `/usr/bin/time -v` for peak RSS.
  6. Writes the JSON / CSV outputs under `target/iub-3/`.
- **Env gates.** Reuse `OC_RSYNC_BENCH_IOURING_RING=1` plus a new
  `OC_RSYNC_BENCH_HIGH_IOPS=1` for the 1M cell (matches the existing
  `OC_RSYNC_BENCH_LARGE=1` / `OC_RSYNC_BENCH_NVME_DATA_PATH=1`
  escalation ladder from IUB-1 observation 3). 100K cell runs under
  the standard `OC_RSYNC_BENCH_IOURING_RING=1` gate only.
- **Host prerequisites.** Same NVMe assumption as IUB-2; fixture
  generation refuses tmpfs (statfs check) and refuses fixture parents
  with `< 4 * payload_size` free disk. 1M cell wants > 6 GiB free
  (fixture + sentinel + headroom + per-iter dest tree).
- **Wall-clock budget.** Full grid (12 cells, hyperfine 10 runs each,
  plus perf + time -v passes) should fit in < 60 min on a Linux NVMe
  dev host. The 1M cold runs dominate; budget ~30 s per run x
  10 runs x 6 cells (1M scale) = ~30 min for the 1M scale alone.
  Cold runs include the drop_caches stall, which adds ~5 s per run
  at the 1M scale.

After IUB-4 + IUB-5 land the harness and cells, IUB-6 / IUB-7 / IUB-8 /
IUB-9 produce baseline numbers and IUB-10 synthesises the joint IUB-2 +
IUB-3 payoff matrix that informs the next io_uring scoping decision.
