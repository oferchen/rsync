# MAP_POPULATE for mmap'd basis files

Tracking issue: oc-rsync task #1663.

## Summary

This audit evaluates whether oc-rsync should add `MAP_POPULATE` to the
`mmap(2)` flags used for read-only basis-file mappings (the bytes the receiver
copies from when reconstructing a destination file from delta tokens). The
goal is to eliminate the per-page minor faults that occur on first touch from
a worker thread, which can stall the `io_uring` SQPOLL kernel thread when the
matching read crosses a fault boundary.

The conclusion is that `MAP_POPULATE` is the wrong instrument for this job.
It pre-faults the entire mapping synchronously at `mmap` time, which is
proportional to the basis-file size rather than to the bytes oc-rsync will
actually read. Delta transfer typically touches only 30-60% of a basis (the
matched blocks), so 40-70% of the pre-fault work is wasted on cold pages we
never read. A targeted `madvise(MADV_WILLNEED, range)` issued just before the
worker submits the io_uring read covers the same fault-elimination goal at a
fraction of the RSS and wall-clock cost. The recommendation is to defer
`MAP_POPULATE` and instead introduce a per-range willneed hint co-located
with the `io_uring` submission site.

## 1. Background

### MAP_POPULATE

`MAP_POPULATE` was added in Linux 2.5.46 and is documented in `mmap(2)`. When
present in the flags argument (only meaningful for `MAP_PRIVATE` mappings
prior to 2.6.23, both `MAP_PRIVATE` and `MAP_SHARED` after), the kernel walks
the requested range at mapping time, allocates the page-cache pages, and
populates the process page tables. The effect is that subsequent loads from
the mapping do not take a minor fault: the PTEs are already valid.

Key properties:

- **Synchronous.** The cost is paid inside the `mmap` syscall, before any
  user code touches the bytes. A 1 GiB mapping requires 262 144 page-table
  entries on 4 KiB pages.
- **Best-effort.** `mmap(2)` documents that on failure to populate
  (e.g. memory pressure, filesystem latency), the call still succeeds and the
  unmapped pages take faults the normal way. Callers cannot assume every
  page is resident on return.
- **No-op on systems lacking the flag.** macOS, FreeBSD, NetBSD, OpenBSD,
  Solaris, and Windows do not define `MAP_POPULATE`. Portable code must
  conditionally compile the flag and degrade gracefully.
- **MAP_LOCKED is distinct.** `MAP_POPULATE` populates the page table but
  does not lock pages in RAM. Pages may still be reclaimed under pressure
  unless `mlock(2)` is also used.

### madvise(MADV_WILLNEED)

`madvise(MADV_WILLNEED, addr, len)` is documented in `madvise(2)`. It hints
that the named range will be accessed soon and asks the kernel to begin
asynchronous read-ahead. Critically, the call returns before the pages are
resident; the kernel queues read-ahead I/O and the caller continues. From
the caller's perspective the pages may still take a fault, but the fault
typically completes by serving an already-in-flight page-cache read rather
than blocking on a fresh I/O.

Differences from `MAP_POPULATE`:

| Aspect | `MAP_POPULATE` | `MADV_WILLNEED` |
|---|---|---|
| When | At `mmap` time | After mapping, any time |
| Sync vs async | Synchronous, blocks `mmap` | Asynchronous, returns fast |
| Range | Whole mapping | Caller-chosen sub-range |
| Page-table work | Up front, all pages | Lazy, kernel-paced |
| Failure mode | Silent partial population | Silent partial read-ahead |
| Portability | Linux only | POSIX (semantics differ across platforms) |

The two mechanisms are complementary, not interchangeable:
`MAP_POPULATE` is a one-shot "do all of it now"; `MADV_WILLNEED` is a
per-range "start fetching this slice now."

### Other prefetch primitives

- `madvise(MADV_RANDOM)` disables file read-ahead for the mapping. It does
  *not* pre-fault. It tells the kernel that a sequential pre-fetch heuristic
  would be wasteful for this access pattern. Useful for sparse delta-match
  scans where neighbouring pages are unlikely to be needed.
- `madvise(MADV_SEQUENTIAL)` requests aggressive read-ahead. Equivalent
  semantically to `posix_fadvise(POSIX_FADV_SEQUENTIAL)` but applied to the
  mapping rather than the file descriptor.
- `posix_fadvise(fd, off, len, POSIX_FADV_WILLNEED)` issues the same
  asynchronous prefetch hint at the file-descriptor level. It works without
  a mapping, and it is callable on the `File` we used to build the
  `MmapReader`. Behaviour is equivalent to `MADV_WILLNEED` on Linux; on
  other platforms (macOS, *BSD) coverage is uneven.

## 2. Current state

`crates/fast_io/src/mmap_reader.rs` is the single point at which oc-rsync
maps a basis file. The relevant call sites are:

- Line 24: `use memmap2::{Mmap, MmapOptions};` - the `memmap2` crate is the
  abstraction over `mmap(2)`/`MapViewOfFile`.
- Line 84: `let mmap = unsafe { MmapOptions::new().map(&file)? };` -
  `MmapOptions::new()` builds a default `MmapOptions` with `len = None`,
  `offset = 0`, `populate = false`, no `huge` flag. `map(&file)` calls
  `mmap` with `PROT_READ`, `MAP_SHARED`, no extra flags. There is no
  `populate(true)` call on the builder.
- Lines 124-128: `advise_sequential` calls `Mmap::advise(Advice::Sequential)`
  (`madvise(MADV_SEQUENTIAL)`).
- Lines 131-135: `advise_random` calls `Mmap::advise(Advice::Random)`
  (`madvise(MADV_RANDOM)`).
- Lines 138-143: `advise_willneed` calls `Mmap::advise_range(Advice::WillNeed,
  offset, len)` (`madvise(MADV_WILLNEED)` over a sub-range). This is the
  per-range willneed hook the recommendation builds on; it exists, it is
  callable, and it is currently unused on the basis-file path.

`memmap2::MmapOptions::populate(true)` would set the internal flag that, on
Linux, ORs `MAP_POPULATE` into the `mmap` flags. We do not call it. The
mapping therefore takes minor faults lazily, one per 4 KiB page, the first
time a worker touches the byte.

`crates/fast_io/src/mmap_reader_stub.rs` is the no-mmap stub used on
platforms where `memmap2` is unavailable; it is a `pub fn open(...) -> Err`
shim and is not affected by this audit.

`crates/transfer/src/map_file/mmap.rs` (line 17, 36-40) is the only consumer
of `MmapReader` on the basis-file path; it constructs `MmapReader::open(path)`
and exposes `map_ptr(offset, len) -> &[u8]` to the delta-application loop.
It does not currently invoke `advise_willneed` before reads.

`crates/checksums/src/parallel/files.rs` opens basis files with `MmapReader`
for parallel checksum scans during signature generation; the same fault
behaviour applies there.

`crates/engine/src/local_copy/prefetch.rs` exposes `advise_sequential_read`
and `advise_dontneed` over the `posix_fadvise` API, gated on
`target_os = "linux"`/`"android"`. There is no current call site for
`POSIX_FADV_WILLNEED`. Any new pre-fault work should reuse this module's
gating pattern.

## 3. Cost analysis

### Committed RSS

`MAP_POPULATE` populates page-table entries proportional to the mapping
length, regardless of how much of the file the caller will actually read.
For 4 KiB pages on `x86_64`/`aarch64` Linux:

| Basis size | Pages populated | PTE memory (8 B/PTE) | Worst-case RSS delta |
|---|---|---|---|
| 1 GiB | 262 144 | ~2 MiB | up to 1 GiB |
| 10 GiB | 2 621 440 | ~20 MiB | up to 10 GiB |
| 100 GiB | 26 214 400 | ~200 MiB | up to 100 GiB |

The "worst-case RSS delta" column assumes the kernel fully populates the
mapping; under memory pressure the kernel will populate fewer pages and the
remainder will fault on demand. On a 16 GiB host, a 100 GiB basis with
`MAP_POPULATE` is undefined-by-design: the kernel does its best, but the
caller cannot rely on any particular outcome. By contrast, the lazy-fault
baseline grows RSS only with what the worker actually reads. For a delta
match rate of 50%, lazy-fault peaks near 50 GiB and `MAP_POPULATE` peaks
near 100 GiB.

The 100 MiB / 1 GiB / 10 GiB cases are the realistic operating range for
backup workloads (large database dumps, VM images, photo libraries). The
100 GiB case is the long-tail case (single-file archives, raw disk dumps).

### Wall-clock cost of pre-fault vs lazy-fault

A minor fault on Linux costs roughly 1-3 us depending on TLB state and
contention. A 1 GiB pre-fault therefore costs on the order of 0.25-0.75 s
of synchronous `mmap`-time work, paid before the first delta byte is read.
Lazy-fault amortises that same cost over the duration of the transfer,
overlapped with checksum work and I/O. For a transfer that copies
500 MiB out of 1 GiB at 200 MiB/s, the lazy faults add a small constant per
read-block; the pre-fault adds 0.25-0.75 s of wall-clock time up front and
pays for the 500 MiB we never touch.

The break-even is workload-dependent:

- If the worker reads 100% of the basis (rare; signature recompute, full
  checksum pass), pre-fault and lazy-fault perform similar total work, and
  pre-fault may win by avoiding fault-handler overhead per access.
- If the worker reads under ~70% of the basis, lazy-fault wins because
  `MAP_POPULATE` does work the transfer never benefits from.

### Interaction with delta transfer

oc-rsync's delta-application path (`crates/transfer/src/map_file/mmap.rs::
MmapStrategy::map_ptr`) is driven by COPY tokens from the sender. Each COPY
token names a `(offset, len)` slice of the basis. The receiver reads exactly
those slices. Empirically (cf. perf-roadmap and benchmark notes) the matched
fraction across a representative corpus sits in the 30-60% range for
incremental backups, with a long tail near 90%+ for single-block edits and
near 5-10% for re-encoded media.

`MAP_POPULATE` cannot exploit this distribution because it operates at
mapping time, before any token is parsed. A per-range
`madvise(MADV_WILLNEED)` issued at COPY-token time touches exactly the
slice we are about to read, scaling with delta hit rate rather than basis
size.

The signature-generation path (`crates/checksums/src/parallel/files.rs`) is
the closer analogue to a "read 100%" case: rolling-checksum scans walk the
entire basis. Even there, sequential read-ahead via
`madvise(MADV_SEQUENTIAL)` (already wired up in `advise_sequential`) is
expected to outperform `MAP_POPULATE` because the kernel prefetches a
sliding window and the work overlaps with checksum CPU.

## 4. Alternatives

| Mechanism | Granularity | Sync vs async | RSS impact | Best for |
|---|---|---|---|---|
| `mmap(MAP_POPULATE)` | Whole mapping | Sync | Up to file size | Read-100% workloads with cheap RAM |
| `madvise(MADV_WILLNEED, range)` | Sub-range | Async | Range size only | Delta COPY tokens, just-in-time |
| `madvise(MADV_RANDOM)` | Whole or sub-range | Async hint | None | Disable read-ahead for sparse access |
| `madvise(MADV_SEQUENTIAL)` | Whole or sub-range | Async hint | Read-ahead window | Sequential scans (signature pass) |
| `posix_fadvise(FADV_WILLNEED)` | FD sub-range | Async | Range size only | Pre-fault before mapping exists |
| `madvise(MADV_HUGEPAGE)` | Aligned range | Hint | Page-size dependent | Large mappings on THP kernels |
| `mlock` / `MAP_LOCKED` | Range | Sync | Hard pin | Latency-sensitive critical paths |

The `MmapReader` API already exposes the three madvise primitives we need:
`advise_random`, `advise_sequential`, `advise_willneed(offset, len)`
(lines 124-143 of `mmap_reader.rs`). The delta-apply call site does not yet
use them; the signature-scan call site uses none of them either, even
though `advise_sequential` would be a natural fit there.

## 5. Decision matrix

For each `(file_size_bucket, io_uring_active, expected_block_match_rate)`
combination, the recommended prefetch strategy is:

| Basis size | io_uring | Match rate | Recommended strategy |
|---|---|---|---|
| < 1 MiB | any | any | None. Below mmap threshold (`MMAP_THRESHOLD = 64 KiB` plus per-file overhead amortisation); buffered I/O dominates. |
| 1 MiB - 1 GiB | off | < 70% | `madvise(MADV_WILLNEED, range)` per COPY token. |
| 1 MiB - 1 GiB | off | >= 70% | `madvise(MADV_SEQUENTIAL)` whole mapping + per-range willneed. |
| 1 MiB - 1 GiB | on | any | `madvise(MADV_WILLNEED, range)` issued *immediately before* the io_uring submission. Avoids SQPOLL stalls without blocking on `mmap`. |
| 1 - 10 GiB | off | < 50% | `madvise(MADV_WILLNEED, range)` per COPY token. `MADV_RANDOM` on the mapping to suppress wasteful read-ahead. |
| 1 - 10 GiB | on | any | Per-range `MADV_WILLNEED` at submission time. Do not use `MAP_POPULATE`: 10 GiB synchronous pre-fault is unacceptable in the `mmap` critical section. |
| 10 - 100 GiB | any | any | Per-range `MADV_WILLNEED`. `MAP_POPULATE` is unsafe (RSS pressure, OOM risk). |
| > 100 GiB | any | any | Per-range `MADV_WILLNEED` only; consider chunked re-mmap to bound resident set. |

The only row where `MAP_POPULATE` is even arguably attractive is the
`< 1 GiB, signature scan, 100% read` corner, and even there the
`MADV_SEQUENTIAL`-driven kernel read-ahead is competitive without the
synchronous penalty.

## 6. Findings

### F1 (HIGH): basis-file mappings take avoidable per-page faults under io_uring SQPOLL

- **Evidence:** `crates/fast_io/src/mmap_reader.rs:84` calls
  `MmapOptions::new().map(&file)` with no `populate` and no madvise. The
  delta-apply consumer in `crates/transfer/src/map_file/mmap.rs:50-66`
  returns slices from the mapping without first hinting the kernel to
  page them in. Under io_uring SQPOLL the kernel polling thread can stall
  on minor-fault resolution when the worker submission queue references a
  not-yet-resident page.
- **Impact:** Latency spikes on the SQPOLL critical path; reduced
  effective queue depth; throughput regressions on cold-cache transfers.
- **Recommended fix:** Add a per-range `madvise(MADV_WILLNEED, offset,
  len)` call in `MmapStrategy::map_ptr` (or the io_uring submission site),
  *before* the io_uring SQE is submitted. The hook
  (`MmapReader::advise_willneed`) already exists at lines 138-143.

### F2 (MEDIUM): `MAP_POPULATE` is the wrong knob for the SQPOLL stall problem

- **Evidence:** Linux `mmap(2)` man page; cost analysis in section 3.
  `MAP_POPULATE` is synchronous and proportional to the *mapping length*,
  while the SQPOLL stall is per-*read-range*. Pre-faulting an entire
  10 GiB basis to avoid faults on the 4 GiB the worker actually reads
  wastes 6 GiB of work and inflates RSS by the same amount.
- **Impact:** Choosing `MAP_POPULATE` to fix this would regress wall-clock
  on workloads with < 70% match rate (the common case for incremental
  backups) and risk OOM on memory-constrained hosts with large basis
  files.
- **Recommended fix:** Do not enable `MAP_POPULATE`. Document the choice
  in `mmap_reader.rs` to prevent re-litigation.

### F3 (MEDIUM): signature scan reads 100% of basis without `MADV_SEQUENTIAL`

- **Evidence:** `crates/checksums/src/parallel/files.rs` opens
  `MmapReader` instances for rolling-checksum passes but does not call
  `advise_sequential`. The kernel default read-ahead window is 128 KiB,
  which under-prefetches relative to the 1 MiB-ish working set of a
  parallel checksum scan.
- **Impact:** Sub-optimal read-ahead during signature generation; minor
  faults serialise behind synchronous I/O on cold caches.
- **Recommended fix:** Call `MmapReader::advise_sequential` after
  `MmapReader::open` on the signature-scan path. The hook exists at
  lines 124-128.

### F4 (LOW): no `posix_fadvise(FADV_WILLNEED)` companion in `prefetch.rs`

- **Evidence:** `crates/engine/src/local_copy/prefetch.rs:38-56` exposes
  `POSIX_FADV_SEQUENTIAL` and `POSIX_FADV_DONTNEED` but no
  `POSIX_FADV_WILLNEED`. The willneed hint at the FD level is the natural
  pre-mmap counterpart for callers that want to start I/O before
  constructing an `MmapReader`.
- **Impact:** A small ergonomics gap; not load-bearing today but useful
  for future optimisations that need to start I/O during file-list
  expansion.
- **Recommended fix:** Add an `advise_willneed_range(file, offset, len)`
  helper in the same module, gated on
  `target_os = "linux"`/`"android"` and a no-op elsewhere, mirroring the
  existing pattern.

### F5 (LOW): no documented decision in source comments

- **Evidence:** `mmap_reader.rs` lines 17-21 explain the safety contract
  but do not mention prefetch or fault behaviour. Future contributors
  attempting to "optimise" the mapping may add `MAP_POPULATE` without the
  context developed here.
- **Impact:** Risk of regression-by-good-intent.
- **Recommended fix:** When the F1 fix lands, add a one-line module-level
  comment pointing at this audit and naming the chosen strategy
  (per-range willneed). No code change is required for this audit.

## 7. Recommendation

Defer `MAP_POPULATE`. Track the SQPOLL fault-stall problem with a per-range
`madvise(MADV_WILLNEED, offset, len)` call, issued by the basis-file consumer
just before the io_uring submission references the slice. The infrastructure
already exists in `MmapReader::advise_willneed_range` (effectively
`advise_willneed` at `mmap_reader.rs:138-143`); the missing piece is a call
site in `crates/transfer/src/map_file/mmap.rs` and in the io_uring submission
path of `crates/fast_io/src/io_uring/`.

Concrete next actions (out of scope for this audit):

- [ ] #1664 wire `advise_willneed` into `MmapStrategy::map_ptr` so each
  COPY-token slice is pre-faulted asynchronously before the read.
- [ ] #1665 wire `advise_sequential` into the signature-scan path in
  `crates/checksums/src/parallel/files.rs`.
- [ ] #1666 add `advise_willneed_range` to `crates/engine/src/local_copy/
  prefetch.rs` for callers that want the FD-level hint before mapping.
- [ ] #1667 add a microbenchmark in `crates/fast_io/benches` that compares
  lazy-fault, `MAP_POPULATE`, and per-range `MADV_WILLNEED` on a 1 GiB
  basis with 50% match rate, to lock in the conclusion empirically.

## References

- `crates/fast_io/src/mmap_reader.rs` lines 24, 84, 124-143
  (`MmapOptions::new().map`, `advise_*` hooks).
- `crates/fast_io/src/mmap_reader_stub.rs` (stub for non-mmap platforms).
- `crates/transfer/src/map_file/mmap.rs` lines 17, 36-66
  (`MmapStrategy::open`, `map_ptr`).
- `crates/checksums/src/parallel/files.rs` (signature-scan basis mapping).
- `crates/engine/src/local_copy/prefetch.rs` lines 38-74
  (`posix_fadvise` wrappers).
- `crates/fast_io/src/io_uring/buffer_ring.rs` (io_uring submission path
  context).
- Linux man pages: `mmap(2)`, `madvise(2)`, `posix_fadvise(2)`.
- `memmap2` crate `MmapOptions::populate` builder method.
- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no `MAP_POPULATE` usage; upstream uses `mmap` + plain reads in
  `fileio.c::map_file`).
