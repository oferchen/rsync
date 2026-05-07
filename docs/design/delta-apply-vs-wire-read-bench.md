# Delta-Apply vs Wire-Read Time Ratio Benchmark Plan (#1080)

Tracking issue: oc-rsync task #1080. Documentation-only design note;
no code lands in this PR. The goal is to decide whether parallel
delta application is worth pursuing on the receiver, or whether the
wire is already the dominant cost.

## 1. Receiver Pipeline

The receiver runs four logical phases per file. They are pipelined
across files via a SPSC channel between the network reader and a
background disk-commit thread (`crates/transfer/src/pipeline/spsc.rs`).

| Phase | Code anchor | Work |
|---|---|---|
| Socket read | `crates/transfer/src/reader/` (`ServerReader<R>`) | demux `MSG_DATA` frames, drain stderr/info into log channels |
| Demux + token decode | `crates/transfer/src/transfer_ops/streaming.rs:74` (`process_file_response_streaming`), `token_loop.rs` | decompress (zlib/zstd), split into `DeltaToken::{Literal,BlockRef,End}` |
| Delta apply | `crates/transfer/src/delta_apply/applicator.rs:221` (`apply_literal`), `:261` (`apply_block_ref`), `:326` (`apply_token`); `delta_apply/checksum.rs`, `delta_apply/sparse.rs` | rolling+strong checksum verify, mmap or 256 KiB sliding-window basis copy, sparse zero-run detection, write to temp |
| Disk commit | `crates/transfer/src/disk_commit/thread.rs:172` (`disk_thread_main`), `process.rs:32` (`process_file`) | optional fsync, rename, metadata + ACL/xattr apply, backup |

Pipeline driver: `crates/transfer/src/receiver/transfer/pipeline.rs:38`
(`run_pipeline_loop_decoupled`). Socket read and demux run on the
foreground thread; delta apply and disk commit run on the disk thread.
There is no instrumentation today: `grep Instant` returns zero hits in
both `delta_apply/` and `transfer_ops/streaming.rs`.

## 2. Question Under Test

Which side is the receiver bottleneck per workload?

- If **wire read + demux > 70%**, the link or the sender is the limit.
  Adding parallel delta apply just shifts idle time onto more cores.
  The optimisation budget belongs in compression, registered buffers,
  or fewer round-trips.
- If **delta apply > 30%**, the basis-side memory copy plus checksum
  verify is on the critical path. A worker pool that fans out token
  streams across cores (one per in-flight file, bounded by
  `min(num_cpus, max_inflight)`) pays off, since each apply call has
  its own basis `MapFile` and `ChecksumVerifier`.
- The answer is workload-dependent, so the bench must sweep both
  axes (basis match rate, file size mix, compression on/off).

## 3. Bench Plan

`crates/transfer/benches/receiver_phase_breakdown.rs` (new file,
criterion + custom harness). Phase timers gate behind a `phase_timing`
feature so release builds carry zero overhead.

- Wrap each of the four phases with a `PhaseTimer` that records
  `Instant::now()` deltas into a per-file `PhaseTimings` struct
  (`socket_ns, demux_ns, apply_ns, commit_ns`). Aggregate across files
  into a `PhaseHistogram` with p50/p95/p99 and total share-of-time.
- Hooks: socket read at the `ServerReader::read` entry, demux at
  `TokenReader::next_token`, apply at `DeltaApplicator::apply_token`,
  commit at `disk_commit::process::process_file`. Timer enters on
  call, exits on return; nested calls sum into the innermost phase.
- Loopback harness over Unix domain sockets so the wire is not the
  variable: a controlled sender thread feeds canned token streams to
  a real `ReceiverContext`. A second arm uses a localhost daemon to
  retain the real codec stack (multiplex, varint NDX, MSG_DATA framing).
- Compression sweep: `--no-compress`, `-z` (zlib), `--zc=zstd`. Each
  matches a separate criterion group.
- Output: per-phase share table, plus an Apply-Share metric
  `apply_ns / (socket_ns + demux_ns + apply_ns + commit_ns)`. Only
  this ratio drives the decision matrix.
- Runner: `cargo bench -p transfer --bench receiver_phase_breakdown`.
  Gated behind `OC_RSYNC_RUN_PHASE_BENCH=1` to keep CI time bounded;
  results land under `target/criterion/` and are summarised in the
  follow-up issue.

## 4. Workloads

Three fixtures, each producing a basis tree and a modified source
tree. The bench script regenerates them from a fixed RNG seed so runs
are reproducible.

| Fixture | Geometry | Mutation |
|---|---|---|
| `small_mostly_changed` | 100 000 files x 4 KiB | 80% of bytes rewritten; high literal share, tiny block-ref share, heavy file-list and demux load |
| `large_mostly_unchanged` | 1 000 files x 100 MiB | 1% of bytes rewritten; block-ref dominates; basis mmap path is hot |
| `monolith` | 1 file x 10 GiB | 5% of bytes rewritten in 64 KiB random windows; single-file pipelining floor; checksum verify and basis copy stress |

Each fixture is run uncompressed and with `-z`. The compressed arm
shifts demux cost up (zlib inflate) and exercises the per-session
zstd context (`token_reader.reset()` between files; the DCtx stays
live for the whole transfer).

## 5. Decision Matrix

The decision rule keys off Apply-Share aggregated over all files in
the fixture (excluding the first 2% as warm-up).

| Apply-Share | Action |
|---|---|
| < 15% | Network or sender is the bottleneck. Do not parallelise apply. Track demux cost; consider compression and registered-buffer work. |
| 15-30% | Marginal. Re-bench after any sender-side speedup; do not start parallel apply yet. |
| 30-50% | Parallel apply pays off; build a bounded worker pool keyed off `max_inflight` files. |
| > 50% | Apply is the dominant cost; parallel apply is mandatory and SIMD checksum work should also be re-audited. |

Failure conditions that void a run:

1. Wire read p99 / p50 ratio > 5x. Indicates external interference
   (kernel scheduler, neighbour load); rerun on an idle host.
2. Disk commit share > 40% on `large_mostly_unchanged`. Indicates the
   tempfile or fsync path dominated; rerun with `--no-fsync` and a
   tmpfs target so the bench measures apply, not disk.
3. Compression-on Apply-Share lower than compression-off by more
   than 10pp on the same fixture. Indicates the demux phase absorbed
   apply work via inlining; verify timer placement and re-run.
