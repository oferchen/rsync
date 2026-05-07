# Delta-apply vs wire-read time-ratio benchmark plan

Tracking issue: oc-rsync task #1080. Sibling audits:
[`docs/audits/madvise-willneed-prefault.md`](madvise-willneed-prefault.md)
(task #1662), [`docs/audits/mmap-iouring-co-usage.md`](mmap-iouring-co-usage.md)
(task #1660).

Last verified: 2026-05-07 against master. Files spot-checked:
`crates/transfer/src/delta_apply/mod.rs`,
`crates/transfer/src/delta_apply/applicator.rs`,
`crates/transfer/src/delta_apply/sparse.rs`,
`crates/transfer/src/reader/multiplex.rs`,
`crates/protocol/src/multiplex/reader.rs`,
`crates/protocol/src/multiplex/io.rs`.

## Scope

Quantify the wall-clock split between two halves of the receiver hot
path so we can decide whether further pipeline parallelism is worth
the engineering cost:

1. **Wire-read** - demultiplex `MSG_DATA` envelopes off the socket, hand
   raw bytes to `apply_token`.
2. **Delta-apply** - decode the 4-byte token, copy literal bytes (or
   `pread` from the basis), update the rolling MD5/XXH3 verifier, run
   sparse-zero detection, write to the destination file.

This is a documentation-only plan. No Rust code is added here. All
benchmark code, harness, and result tables route through later
follow-up tasks once the methodology in this doc is signed off.

## TL;DR

The receiver loop in `transfer::delta_apply::applicator::apply_token`
serialises wire-read against delta-apply: every token waits on the
`MultiplexReader` for its 4-byte header plus payload, then runs the
applicator in the same thread before pulling the next header. There is
no overlap today. A criterion harness covering three basis sizes (1 MB
/ 100 MB / 1 GB) and three match rates (5 % / 50 % / 95 %) is the
cheapest way to learn the wire-vs-apply ratio at every realistic
operating point. The expected ratios drive the parallelism decision
(see [Decision matrix](#decision-matrix)).

## 1. Where the work happens today

### 1.1 Delta-apply

Single canonical site: the
[`DeltaApplicator`](../../crates/transfer/src/delta_apply/applicator.rs)
and its three helpers:

- `crates/transfer/src/delta_apply/applicator.rs:328`
  (`apply_token`) - reads a 4-byte little-endian token from the
  multiplex reader, dispatches to either `apply_literal` (positive
  token = literal length) or `apply_block_ref` (negative token = block
  index `-(token+1)`). Mirrors upstream `receiver.c:240` `receive_data`.
- `crates/transfer/src/delta_apply/applicator.rs:220`
  (`apply_literal`) - hashes the literal into the
  [`ChecksumVerifier`](../../crates/transfer/src/delta_apply/checksum.rs)
  and writes via `SparseWriteState` or directly through `write_all`.
- `crates/transfer/src/delta_apply/applicator.rs:261`
  (`apply_block_ref`) - resolves block index to `(offset, len)` against
  the cached `MapFile`, reads `block_data` via `basis_map.map_ptr`,
  hashes, writes.
- `crates/transfer/src/delta_apply/applicator.rs:390`
  (`finish`) - drains the trailing whole-file checksum and verifies
  against the local digest; mirrors `receiver.c:408`.
- `crates/transfer/src/delta_apply/sparse.rs:63`
  (`SparseWriteState::write`) - 32 KB chunked leading/trailing-zero
  detection (matching upstream `CHUNK_SIZE`). Holes become `seek`
  calls, non-zero runs become `write_all`.

Token loop entry-points that drive the applicator from the receiver
side:

- `crates/transfer/src/transfer_ops/token_loop.rs` - inline-blocking
  token loop used by the synchronous receiver pipeline.
- `crates/transfer/src/transfer_ops/streaming.rs` - streamed variant
  used when the destination writer is io_uring-backed.
- `crates/transfer/src/pipeline/receiver.rs` - SPSC-decoupled receiver
  (see `crates/transfer/src/pipeline/spsc.rs`).
- `crates/transfer/src/disk_commit/process.rs` - delta-apply running
  on the disk-commit thread.

### 1.2 Wire-read

Two cooperating layers, both touched by every byte the applicator
consumes:

- `crates/protocol/src/multiplex/reader.rs:72` (`MplexReader`) -
  generic `Read` adapter that pulls 4-byte envelope headers via
  `recv_msg_into` (`crates/protocol/src/multiplex/io.rs`) and serves
  payload bytes to the caller. Used directly on the SSH and TCP
  transports.
- `crates/transfer/src/reader/multiplex.rs:28` (`MultiplexReader`) -
  receiver-side wrapper that adds `MSG_IO_ERROR`, `MSG_NO_SEND`,
  `MSG_REDO`, `MSG_ERROR_EXIT`, batch-tee, and the zero-copy
  `try_borrow_exact` fast path
  (`crates/transfer/src/reader/multiplex.rs:239`).

Both readers default to a 32 - 64 KB staging buffer matching upstream
`IO_BUFFER_SIZE`. Every `apply_token` call takes the demux path: a 4-byte
header `read_exact` followed by either a 4 - 32 KB literal payload or a
zero-payload block-reference token.

## 2. Cost model

Per token, in the order the work happens on the receiver thread:

| Step | Where | Dominant cost |
| --- | --- | --- |
| Envelope header read | `protocol/src/multiplex/io.rs` `recv_msg_into` | One `recv` syscall amortised across the 32 - 64 KB staging buffer. ~50 - 200 ns when buffered, full RTT/page when the buffer is empty. |
| Token decode | `delta_apply/applicator.rs:328` | 4-byte LE decode + branch. Negligible (single-digit ns). |
| Literal payload copy | `delta_apply/applicator.rs:354-374` | `read_exact` into `TokenBuffer` (reused across tokens), then `write_all` to the destination. Memory-bandwidth bound at ~6 - 12 GB/s on modern hardware. |
| Block-reference fault | `delta_apply/applicator.rs:306` `basis_map.map_ptr` | First touch on a 4 KB basis page on Unix `AdaptiveMapStrategy` triggers a minor page fault (~1 - 5 us); cold pages cost a major fault (100 us - 10 ms). On `BufferedMap` (Windows, io_uring writer) each `map_ptr` issues a `pread` (~200 ns - 5 us per cache line of basis). |
| Checksum update | `delta_apply/applicator.rs:308` `ChecksumVerifier::update` | MD5/XXH3 streaming hash. XXH3 ~6 GB/s, MD5 ~600 MB/s on aarch64. Touches every byte. |
| Sparse-zero detection | `delta_apply/sparse.rs:63` | 32 KB chunks, `leading_zero_count` + `trailing_zero_count` from `crates/transfer/src/constants.rs` (16-byte `u128` SIMD scan). ~0.5 - 1 cycle per byte; only active under `--sparse`. |
| Output write | `delta_apply/applicator.rs:312` | Either buffered `write_all` or `IoUringWriter` SQE. Without `--fsync` no per-token syncing happens. |
| Trailing fsync | `delta_apply/applicator.rs:390` `finish` + `disk_commit/process.rs` | Once per file. 100 us - 50 ms depending on filesystem and sync policy. Excluded from per-token timing; tracked as a per-file overhead. |

Two non-obvious cross-layer effects to capture in the model:

- **Basis-mmap fault stall.** Every cold block reference is a synchronous
  page fault on the receiver thread. On large basis files (1 GB) and
  high-match-rate workloads (95 %) the fault rate dominates. See
  [`docs/audits/madvise-willneed-prefault.md`](madvise-willneed-prefault.md)
  for the existing analysis.
- **Buffer boundary copies.** When a literal token straddles two
  `MultiplexReader` frames the zero-copy fast path
  (`reader/multiplex.rs:239 try_borrow_exact`) returns `None` and the
  applicator falls back to a copy through `TokenBuffer`. The fraction
  of straddling tokens is a function of token-size distribution vs the
  64 KB envelope; the harness must report it.

## 3. Methodology

### 3.1 Harness

A new criterion bench, sibling to the existing
`crates/engine/benches/delta_transfer_benchmark.rs`. Working name:
`crates/transfer/benches/delta_apply_vs_wire_read.rs`. The bench has
three groups:

1. `wire_read_only` - drains a pre-recorded multiplex byte stream
   through `MultiplexReader::read_exact` into a sink. No applicator.
   Establishes the wire-read baseline.
2. `delta_apply_only` - feeds an in-memory `Cursor<Vec<u8>>` of the
   same byte stream straight into `apply_delta_stream`. No multiplex
   demux. Establishes the apply baseline.
3. `end_to_end` - the production path: real `MultiplexReader` wrapping
   a `Cursor` of the wire bytes, real `DeltaApplicator`. The
   end-to-end value is the one we ship; the ratio against the two
   baselines isolates pipeline overhead.

Each group is parameterised over basis size and match rate (see
[3.2](#32-parameter-grid)). All three groups share the same fixture
cache to keep variance low.

### 3.2 Parameter grid

Three basis sizes:

- `1 MB` - fits in L2 / L3, tests the literal-copy and checksum loop in
  isolation. mmap is at the `AdaptiveMapStrategy` 1 MiB threshold so
  this also exercises the buffered basis path.
- `100 MB` - exceeds L3 on every shipping CPU; basis pages alternate
  between cold and warm. Representative of typical large files.
- `1 GB` - exceeds DRAM bandwidth budget; cold-fault rate dominates
  unless `MADV_WILLNEED` lands first.

Three match rates:

- `5 %` - cold start / unrelated content. Token stream is overwhelmingly
  literal; wire-read should dominate.
- `50 %` - mixed edit, the realistic upgrade scenario.
- `95 %` - small append / metadata-only edit. Token stream is dominated
  by short block references; basis-fault and apply costs dominate.

Total grid: 3 * 3 = 9 parameter cells, run for all three groups (27
benchmarks). Token stream is generated once per cell from a synthetic
basis (`generate_basis` in the existing engine bench is reusable).

### 3.3 Wire-byte recording

Each cell needs a deterministic, on-disk multiplex stream. Generation
flow:

1. Build basis + modified buffers via the existing
   `engine/benches/delta_transfer_benchmark.rs::create_test_pair`.
2. Run `engine::delta::generate_delta` against the modified buffer.
3. Serialise the token stream through `protocol::send_msg(MSG_DATA, ..)`
   into a file in `target/bench-fixtures/delta-apply-vs-wire-read/`.
4. Cache by `(basis_size, match_rate, block_size, algorithm)`. The
   harness re-uses cached fixtures across runs.

This keeps wire-read and apply paths bit-identical across the three
groups, which is what makes the ratio meaningful.

### 3.4 Measurement

- Criterion `Throughput::Bytes(basis_size)` so reports are MB/s.
- One warm-up iteration per cell (criterion default).
- 100 measured iterations per cell at the small sizes, 30 at 1 GB to
  keep total wall-clock under 10 minutes.
- Capture three derived metrics in custom criterion measurements:
  - `tokens_per_sec`
  - `straddle_token_fraction` (zero-copy `try_borrow_exact` returned
    `None`)
  - `basis_fault_rate` (Linux only, `getrusage` minor + major faults
    delta around the apply call).
- All sparse-write codepaths exercised by toggling `--sparse` as a
  fourth axis on the 100 MB / 50 % cell only (one extra benchmark, not
  the full grid).

### 3.5 Platform matrix

The harness must run unmodified on each platform we ship; numbers are
collected on:

- Linux x86-64, kernel 6.x, ext4 with `MmapStrategy = AdaptiveMmap`.
- Linux x86-64, kernel 6.x, ext4 with `BasisWriterKind::IoUring` (forces
  `BufferedMap`).
- macOS aarch64, APFS, `AdaptiveMmap` only.
- Windows x86-64, NTFS, `BufferedMap` only.

Cross-platform variance feeds the decision matrix; we expect the
io_uring + `BufferedMap` cell to show the highest wire-read overhead
share because cold-page faults are converted into explicit `pread`
syscalls.

## 4. Expected ratios and what they tell us

Approximate ratios are first-order estimates based on the cost model
in [section 2](#2-cost-model). The harness either confirms or refutes
them; either way the answer narrows the parallelism design space.

| Basis | Match | Wire : apply | Bottleneck | Read |
| --- | --- | --- | --- | --- |
| 1 MB | 5 % | 75 : 25 | Wire-read of literals | Wire-side parallelism (recv-multishot) is the big win. Pipelining apply off the receiver thread saves little: apply is ~25 % of wall time. |
| 1 MB | 50 % | 60 : 40 | Mixed | Modest gain from either side. |
| 1 MB | 95 % | 30 : 70 | Apply (checksum + sparse) | Off-load apply to a worker; wire-side parallelism unhelpful because few wire bytes flow. |
| 100 MB | 5 % | 80 : 20 | Wire-read | Same as 1 MB / 5 % but absolute volume justifies recv-side optimisations (PBUF_RING / fixed buffers). |
| 100 MB | 50 % | 55 : 45 | Balanced | Pipelining with the SPSC queue (already wired in `pipeline/spsc.rs`) should be near-optimal. Useful as a regression baseline. |
| 100 MB | 95 % | 25 : 75 | Apply + basis fault | Prefetch basis (`MADV_WILLNEED`, audit #1662). Pipelining wire-read off the apply thread shows diminishing returns. |
| 1 GB | 5 % | 85 : 15 | Wire-read, network-bound on remote runs | Network throughput is the cap; pipeline overlap recovers no more than the apply share (~15 %). |
| 1 GB | 50 % | 50 : 50 | Balanced | The headline cell for pipeline-parallelism ROI. Below 1.6 x speed-up from pipelining means the lock-free SPSC + worker thread is not worth additional engineering. |
| 1 GB | 95 % | 20 : 80 | Cold basis-page faults | The case madvise-willneed (#1662) and io_uring basis prefetch are designed for. Wire-read mostly idle; off-loading apply to a separate thread is *necessary*, not optional, because the apply thread blocks on faults. |

### Decision matrix

The three follow-up branches the ratios drive:

1. **Apply-side worker (only if apply >= 50 % at the 1 GB / 50 % cell).**
   Promote `pipeline/spsc.rs` to the default code path on every
   transport, not just io_uring writers. Existing infrastructure;
   shipping cost is one boolean.
2. **Wire-side recv-multishot (only if wire-read >= 60 % at any cell).**
   Wire `IORING_OP_RECV_MULTISHOT` against
   [`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md)'s
   PBUF_RING. Pays off proportionally to the wire-read share.
3. **Basis prefetch (always, if 95 % cells fault-bound).** Land
   `MADV_WILLNEED` from
   [`docs/audits/madvise-willneed-prefault.md`](madvise-willneed-prefault.md)
   and re-run the 95 % column. Independent of pipeline parallelism;
   strictly additive.

Cells where neither half exceeds 60 % indicate the receiver is already
balanced; further parallelism is not justified by data.

## 5. Out of scope

- End-to-end transfer benchmarks (covered by
  `engine/benches/delta_transfer_benchmark.rs`).
- Sender-side delta generation cost (different bottleneck, separate
  audit).
- TCP / SSH transport latency. The harness uses an in-memory
  `Cursor<Vec<u8>>` so wire-read time is the *protocol* cost, not the
  network cost. A follow-up task can re-run the same parameter grid
  over a real socket pair to add network-bound numbers.

## 6. Follow-up tasks

- Land the criterion harness described in [3.1](#31-harness). Tracking
  task to be filed once this plan is reviewed.
- Re-run the grid after `MADV_WILLNEED` (task #1662) lands; expect the
  95 % column apply-share to drop by 5 - 10 percentage points.
- Re-run the grid after PBUF_RING recv-multishot (task #2043) lands;
  expect the 5 % column wire-share to drop similarly.
