# Parallel rolling-hash fan-out across files on the delta sender

Tracking issue: oc-rsync task #2048. Branch:
`docs/delta-sender-parallel-rolling-hash-2048`. Distinct-from siblings:
#1023 (intra-file rolling-hash parallelism for one huge file) and #1079
(receiver-side multi-file delta-apply pipeline). This audit is investigation
only; no code is changed.

## Summary

The delta sender today computes `match_sums()` (rolling Adler-style + strong
checksum + token emission) one file at a time inside the NDX-driven request
loop in `crates/transfer/src/generator/transfer.rs:47`. On a workload of many
medium-sized files the loop saturates one CPU on the sender. The wire
protocol does not require strict per-file order: each file record is
self-identified by its NDX prefix, and upstream `receiver.c:recv_files()`
(`target/interop/upstream-src/rsync-3.4.1/receiver.c:560`) calls
`read_ndx_and_attrs()` and dispatches by NDX, not by position. Within a
single file record the bytes must remain a contiguous sequence (NDX, iflags,
sum-head, token stream, trailing whole-file checksum), but distinct file
records can in principle be interleaved on the wire as long as the receiver
sees a complete record per NDX. We already have the building blocks
(`BoundedReorderBuffer`, `WorkQueue` + `drain_parallel`, indexed
`par_chunks` collect) to dispatch rolling+strong-sum computation to a rayon
pool and serialize the wire emission in NDX order via the reorder buffer.
**Recommendation: prototype, do not implement on master.** The expected win
is real on the medium-files workload, but the head-of-line-blocking risk for
mixed sizes, the memory cost of buffering deltas in flight, and the
interaction with `--inplace`, `--append`, `--checksum`, and INC_RECURSE all
need a benchmark gate and a feature flag before any wire-emitting path is
touched.

## Source files inspected

All paths repository-relative.

- `crates/transfer/src/generator/transfer.rs` (NDX-driven sender loop,
  `run_transfer_loop`, `send_files()` mirror).
- `crates/transfer/src/generator/delta.rs` (`generate_delta_from_signature`,
  `stream_whole_file_transfer`, `compute_file_checksum`).
- `crates/transfer/src/generator/protocol_io.rs` (`read_signature_blocks`,
  ndx writer helpers).
- `crates/transfer/src/delta_config.rs` (`DeltaGeneratorConfig`).
- `crates/transfer/src/delta_pipeline.rs`,
  `crates/transfer/src/pipeline/async_signature.rs`,
  `crates/transfer/src/pipeline/pending.rs` (existing pipelined helpers).
- `crates/transfer/src/reorder_buffer.rs` (sequence-tagged
  `BoundedReorderBuffer`, sliding-window backpressure).
- `crates/transfer/src/receiver/transfer/pipeline.rs` (existing
  `par_iter().map().collect()` pattern preserving NDX order).
- `crates/match/src/generator.rs` (`DeltaGenerator::generate`, the rolling
  + hash-search hot loop).
- `crates/match/src/script.rs`, `crates/match/src/index/mod.rs` (delta
  token model and `DeltaSignatureIndex`).
- `crates/checksums/src/rolling/checksum/mod.rs` (`RollingChecksum::update`
  with AVX2 / SSE2 / NEON dispatch and scalar fallback at lines 1-30).
- `crates/checksums/src/rolling/digest.rs` (`RollingDigest::from_bytes`).
- `crates/checksums/src/strong/mod.rs` (strong digest dispatch:
  MD4 / MD5 / XXH64 / XXH3 / XXH3-128).
- `crates/signature/src/parallel.rs` (existing intra-file parallel
  signature generation, `BATCH_SIZE = 16`, `PARALLEL_THRESHOLD_BYTES =
  256 * 1024`).
- `crates/engine/src/concurrent_delta/mod.rs` (the
  receiver-side concurrent pipeline that this audit is explicitly
  contrasted with), `crates/engine/src/concurrent_delta/types.rs`
  (`DeltaWork`, `FileNdx`, `sequence`),
  `crates/engine/src/concurrent_delta/work_queue/{mod,bounded,drain}.rs`.
- Upstream rsync 3.4.1 under `target/interop/upstream-src/rsync-3.4.1/`:
  `sender.c:send_files()`, `match.c:match_sums()`, `match.c:hash_search()`,
  `match.c:matched()`, `match.c:send_token()`, `receiver.c:recv_files()`,
  `generator.c:generate_files()`.

## 1. Current sender model

### 1.1 The NDX-driven loop

The sender (named `generator` in oc-rsync because it generates and emits
delta data; see the role comment in
`crates/transfer/src/delta_transfer.rs:12-22`) runs one transfer loop per
session. The entry point is `GeneratorContext::run_transfer_loop`
(`crates/transfer/src/generator/transfer.rs:47`), which mirrors
`sender.c:send_files()` (`target/interop/upstream-src/rsync-3.4.1/sender.c:199`)
phase-for-phase:

1. Read NDX from receiver (`generator/transfer.rs:138`).
2. Read iflags + trailing fnamecmp / xname (`transfer.rs:227-232`).
3. If transfer not requested, emit itemize and continue
   (`transfer.rs:240-244`). This mirrors `sender.c:286-305`.
4. Read sum-head and signature blocks (`transfer.rs:267,281`,
   delegating to `read_signature_blocks` in `protocol_io.rs`). Mirrors
   `sender.c:347` (`receive_sums()`).
5. If `has_basis`, compute the delta script via
   `generate_delta_from_signature` (`generator/delta.rs:91`), then write
   NDX, iflags, token stream, and trailing file checksum
   (`transfer.rs:316-352`). Mirrors `match.c:362 match_sums()` followed
   by `sum_end()` and the trailing checksum write at
   `match.c:411`.
6. Else stream the whole file via `stream_whole_file_transfer`
   (`generator/delta.rs:199`). Mirrors `sender.c:354-369`'s whole-file
   path.

Steps 5 and 6 happen back-to-back inside the same single-threaded loop
iteration; there is no concurrent dispatch across files.

### 1.2 Where rolling and strong checksums are computed on the sender

- **Rolling checksum on the sender** lives in
  `crates/match/src/generator.rs:81 DeltaGenerator::generate`. The hot
  loop slides the window byte-by-byte
  (`match/src/generator.rs:125-285`), calls
  `RollingChecksum::roll(outgoing, incoming)`
  (`match/src/generator.rs:142`) and `update(slice)` on bulk refills
  (`match/src/generator.rs:254-257`). The rolling primitive itself is
  defined in `crates/checksums/src/rolling/checksum/mod.rs` and dispatches
  to AVX2 / SSE2 / NEON / scalar paths
  (`rolling/checksum/mod.rs:1-30`). After the hash table hits, the
  inner adjacent-match refill loop
  (`match/src/generator.rs:188-281`) recomputes the rolling sum from
  scratch via SIMD-accelerated `update()` mirroring
  `match.c:303-308`.
- **Strong checksum on the sender** is invoked in two distinct places:
  1. **Per-block strong-sum verification** during delta generation, via
     `DeltaSignatureIndex::find_match_slices` /
     `check_block_match_slices` (`crates/match/src/index/mod.rs`,
     called from `match/src/generator.rs:178,182,186`). This computes
     the strong sum of the candidate block window and compares it
     against the receiver-supplied block strong-sum. The strong digest
     dispatch lives in `crates/checksums/src/strong/mod.rs` and
     `crates/checksums/src/strong/strategy/`.
  2. **Whole-file checksum** appended after the token stream, via
     `compute_file_checksum` (`generator/delta.rs:289`) for the delta
     path and `ChecksumVerifier` inside `stream_whole_file_transfer`
     (`generator/delta.rs:217,232,249`) for the whole-file path. Mirrors
     `match.c:370 sum_init()` followed by the per-chunk
     `sum_update()` in `match.c:125-128` and the final
     `sum_end()` at `match.c:411`.

### 1.3 The per-file iteration loop

Per file, the sender executes the whole pipeline serially:
`receive_sums()` -> `do_open_checklinks()` -> `map_file()` ->
`match_sums()` -> `unmap_file()`/`close()` -> next NDX. This mirrors
upstream byte-for-byte
(`target/interop/upstream-src/rsync-3.4.1/sender.c:347-449`). One
sender thread, one CPU. No fan-out across files.

### 1.4 Existing parallelism

The sender already has parallelism in two places, but neither covers the
per-file rolling-hash + strong-sum computation that #2048 targets:

- **Parallel file-list build** on the sender:
  `crates/transfer/src/generator/file_list/walk.rs` and
  `crates/transfer/src/parallel_io.rs` parallelize `lstat` and
  metadata fetches with rayon `par_iter`
  (audited in `crates/engine/src/concurrent_delta/mod.rs:93-108`). This
  is the "stat fan-out" pattern, not the checksum pipeline.
- **Parallel signature generation** inside one file:
  `crates/signature/src/parallel.rs:138 par_chunks(BATCH_SIZE)
  .enumerate().flat_map_iter()` computes block rolling + strong
  digests across rayon threads, with `PARALLEL_THRESHOLD_BYTES =
  256 * 1024` (`parallel.rs:172`) and `BATCH_SIZE = 16`
  (`parallel.rs:133`). Note: this is the **receiver** generating a
  signature from its basis file (see comment at `parallel.rs:1-7`),
  used in
  `crates/transfer/src/receiver/transfer/pipeline.rs:182-200`. It does
  not run on the sender's match path. Tracker #1024 covered this
  signature-side parallelism and is completed for receivers.

In short: nothing on the delta sender's match path runs in parallel
across files today.

## 2. Wire-ordering constraint analysis

### 2.1 What the sender writes per file

Following `target/interop/upstream-src/rsync-3.4.1/sender.c:411-424` and
`match.c:362-424`, one file record is:

1. `write_ndx(f_out, ndx)` - file index (varint NDX, monotonic).
2. `iflags` (2 bytes) and optional fnamecmp_type / xname trailers.
3. `write_sum_head(f_xfer, s)` - block count, block length, remainder,
   strong-sum length (16 bytes).
4. The token stream produced by `match_sums()`. Each `send_token()`
   call emits either a literal-data prefix + bytes
   (`token.c:send_token()` via `match.c:117`) or a back-reference token
   (`-(i+1)`). `match_sums()` always finishes with a final
   `matched(f, s, buf, len, -1)` to flush the EOF token
   (`match.c:407-408`).
5. After the EOF token, the trailing whole-file checksum is written by
   the caller via `sum_end(sender_file_sum)` then
   `write_buf(f_xfer, sender_file_sum, xfer_sum_len)` in
   `match.c:411` and the wrapping send loop. oc-rsync mirrors this in
   `crates/transfer/src/generator/transfer.rs:316-352`.

The token stream for a single file is therefore a contiguous wire
substring framed by `[NDX, iflags, sum_head]` at the start and the
file checksum at the end.

### 2.2 What the receiver tolerates

`target/interop/upstream-src/rsync-3.4.1/receiver.c:554-588 recv_files()`
loops on `read_ndx_and_attrs(f_in, f_out, &iflags, ...)`
(`receiver.c:560`), which sets `cur_flist` from the NDX value
(`io.c:read_ndx_and_attrs()` updates `cur_flist`). The receiver does
not assume the NDX it reads is sequential with the previous one; it
indexes into the file list by `ndx - cur_flist->ndx_start`
(`receiver.c:590-593`), which mirrors the sender's mapping at
`sender.c:263-266`. This is the hook that already supports
INC_RECURSE: sub-list NDX values can arrive in arbitrary order
relative to `cur_flist` boundaries, and the existing dispatch in
oc-rsync tolerates this (`generator/transfer.rs:223-224
wire_to_flat_ndx`).

What the receiver *does* require:

- A complete file record per NDX. Token stream ends with the EOF token
  (`-1` for delta, the per-file final `sum_update()`-then-`sum_end()` for
  whole-file). Mid-record interleaving with another file's token would
  be unrecoverable; the receiver consumes the trailing checksum
  immediately after the EOF token in
  `receiver.c:receive_data()` -> `recv_files()`.
- Phase ordering: phase 1 must complete (sender writes `NDX_DONE`
  marker, `sender.c:252-256`) before phase 2 redos start
  (`sender.c:312-329 csum_length` toggle). This is *cross-phase*
  ordering, not per-file ordering inside a phase.
- Generator-pull ordering: the sender only emits a record for an NDX
  the receiver *asked for* via the inbound NDX read. `generate_files`
  on the receiver side
  (`target/interop/upstream-src/rsync-3.4.1/generator.c:2226`)
  controls which NDXs are requested and in what order.

### 2.3 Conclusion: per-file ordering is not strict

The sender writes records strictly in response to inbound NDX
requests. Within one phase, the inbound NDX stream is whatever order
the receiver chose to pull. There is no protocol-level requirement
that NDX values be monotonic in *outbound* order: monotonicity is
established by the receiver's pull, the sender just answers. So a fan-
out scheme that:

1. Reads a batch of inbound NDX requests (each carrying the receiver's
   sum-head + signature blocks).
2. Dispatches the per-file rolling + strong-sum computation to rayon.
3. Reorders results back to *inbound NDX request order* before writing
   to the wire.

is wire-equivalent to the current loop. The reorder is required to
preserve the receiver's expected response order, not because the
protocol forbids reordering: it is required because the pipelined
receiver (and the multiplex layer) expects the sender to answer pulls
in pull order so it can commit files to the right destinations and
not block on a record that never arrives.

## 3. Existing infrastructure that helps

### 3.1 BoundedReorderBuffer

`crates/transfer/src/reorder_buffer.rs:55 BoundedReorderBuffer<T>`
implements a sliding-window reorder buffer with backpressure. Items are
inserted with a sequence number; insert returns either the contiguous
drain starting at `next_expected` or a `BackpressureError` if the
sequence is outside the acceptance window. Default window
`DEFAULT_WINDOW_SIZE = 64` (`reorder_buffer.rs:26`). The doc comment at
`reorder_buffer.rs:18-22` cites
`receiver.c:recv_files() processes files in file-list order` as the
upstream invariant the buffer protects.

### 3.2 WorkQueue and drain_parallel

`crates/engine/src/concurrent_delta/work_queue/{mod,bounded,drain}.rs`
provides a bounded SPMC channel with a `drain_parallel(f)` that uses
`rayon::scope` to spawn one task per work item with per-thread sharded
buffers (`work_queue/mod.rs:1-50`, `work_queue/drain.rs:14-60`). The
queue's default capacity is `2 * rayon::current_num_threads()`
(`work_queue/mod.rs:38-42`), bounding in-flight items.

`DeltaWork` (`engine/src/concurrent_delta/types.rs:65`) already carries
both an `ndx: FileNdx` and a `sequence: u64` (`types.rs:67-74`), so
producers can stamp monotonic sequence numbers and consumers can
reorder via `ReorderBuffer`. This is identical to the pattern #2048
needs.

### 3.3 The receiver-side concurrent_delta pipeline (#1325, completed)

`crates/engine/src/concurrent_delta/mod.rs` documents the receiver-side
parallel delta-apply pipeline. The producer (network reader on the
receiver) assigns monotonic sequence numbers, rayon workers run
strategies (`WholeFileStrategy`, `DeltaTransferStrategy`,
`engine/src/concurrent_delta/strategy.rs`), and `ReorderBuffer` yields
results in submission order before commit. The audit at
`concurrent_delta/mod.rs:52-166` classifies every `par_iter` site in
the codebase as SAFE / GUARDED / RISK; #2048 must extend this audit
once a sender-side site is added. Note: this pipeline is **receiver-
side**. It runs after the sender has emitted bytes; it does not
parallelize the sender.

### 3.4 The receiver pipeline already has the pattern

`crates/transfer/src/receiver/transfer/pipeline.rs:160-200` runs
`par_iter().map(...).collect()` over a batch of upcoming files,
computing basis-file signatures on rayon threads, then iterates the
collected `Vec` *sequentially* to send signature requests in NDX
order. Comment at `pipeline.rs:179-181`: "Ordering: wire protocol
requires file requests in file-list index order. Preserved by
`par_iter().map().collect()` + sequential zip/send loop below."
This is exactly the shape #2048 would replicate on the sender, with
delta scripts in place of signatures.

## 4. Proposed fan-out design (sketch only)

### 4.1 Pipeline shape

```
Sender NDX-read thread          Rayon pool                  Wire writer
---------------------------     ---------------------       ----------------
read_ndx_and_attrs()            (W workers)                  (single writer)
read sum_head + sig_blocks
  |
  |  stamp sequence = N
  |  build SenderWork {
  |      ndx, sequence,
  |      sum_head, sig_blocks,
  |      source_path, file_size,
  |      iflags, ...
  |  }
  v
WorkQueue (bounded, 2*W)  ────► drain_parallel(|w| {
                                  open source -> read ->
                                  generate_delta_from_signature(w) ->
                                  compute_file_checksum(...)
                                  return SenderResult { sequence,
                                                        ndx, iflags,
                                                        wire_ops, checksum }
                                })
                                          │
                                          ▼
                                BoundedReorderBuffer<SenderResult>
                                  insert(sequence, result)
                                  drain contiguous run
                                          │
                                          ▼
                                Wire writer (ServerWriter):
                                  for each result in drain order:
                                    write_ndx_and_attrs(...)
                                    write_sum_head(...)
                                    write tokens (compressed if any)
                                    write trailing checksum
                                  flush()
```

The NDX-read thread is the single producer (matching the SPMC contract
in `work_queue/mod.rs:11-22`). Workers run
`generate_delta_from_signature` (`generator/delta.rs:91`) and
`compute_file_checksum` (`generator/delta.rs:289`) entirely in
isolation: each worker holds its own source file handle, its own
mmap, and its own `DeltaSignatureIndex`. The wire writer remains
single-threaded; it consumes from `ReorderBuffer` and pushes to the
multiplex layer in NDX-request order.

### 4.2 What the workers compute

Per `SenderWork` item, a worker produces:

1. The reconstructed `FileSignature` from `sig_blocks` (currently done
   inline in `generator/delta.rs:122-148`).
2. The `DeltaSignatureIndex` (`match::index::DeltaSignatureIndex`).
3. The `DeltaScript` from `DeltaGenerator::generate`
   (`match/src/generator.rs:81`).
4. The whole-file checksum from `compute_file_checksum`
   (`generator/delta.rs:289`).
5. The wire token vector via `script_to_wire_delta`
   (current call at `generator/transfer.rs:334`).

Optional compression (`token_encoder`) is intentionally **kept on the
wire writer thread**: upstream uses one zstd / zlib `CCtx` per
session (`generator/transfer.rs:74-80`, comment cites `token.c`), so
the encoder must remain serial. Workers emit uncompressed wire ops;
the writer thread compresses per-record before emission.

### 4.3 Head-of-line blocking risk

A single 50 GiB file at NDX `N` blocks the wire writer until its
delta is fully computed, even if NDX `N+1..N+W` finished first. With
sequence-tagged ordering this is identical to upstream's serial
behaviour for the worst case, but it bounds the gain when file sizes
are heavy-tailed. Three relevant constraints:

- The reorder window (`DEFAULT_WINDOW_SIZE = 64`) caps how many
  successors can pre-compute before backpressure kicks in.
  Downstream of the head-of-line file there is at most `W` items of
  bandwidth pre-built.
- `--inplace` (basis == destination) magnifies HOL: the file we're
  blocking on may also be holding open the destination FD that the
  next file will reuse if `delay_updates` interacts.
- Issues #1883 / #1884 in the tracker are about HOL on the receiver
  side; #2048 introduces a *symmetric* concern on the sender. The
  same mitigation (size-aware scheduling: schedule large files first
  so their tail time is hidden by smaller files in flight) applies.

The design must explicitly skip the fan-out when the inbound NDX
batch contains a single file or when the largest file in the window
exceeds a threshold (e.g. >= 1 GiB), falling back to the serial path.
This mirrors the threshold pattern at
`crates/signature/src/parallel.rs:172 PARALLEL_THRESHOLD_BYTES` and
the receiver-side `parallel_thresholds.signature` at
`crates/transfer/src/receiver/transfer/pipeline.rs:177`.

### 4.4 Memory cost

Per in-flight file the worker holds:

- The signature blocks (`Vec<SignatureBlock>` of `block_count *
  (4 + strong_sum_length)` bytes).
- The `DeltaSignatureIndex` hash table.
- The source-file mmap (`map_file`, currently from
  `generator/delta.rs` callers). Size: file size, but RSS-cheap on
  Linux because pages fault in on demand.
- The accumulated `DeltaScript` token vector, which can hold up to
  `block_len + CHUNK_SIZE` bytes of pending literals before flushing
  (`match/src/generator.rs:148`). Worst case for a no-match file is
  the entire file as literals, but the early-flush at
  `match/src/generator.rs:148` caps live bytes per file.

For `W = 8` workers and a default reorder window of 64, the
worst-case memory increase over the current serial loop is roughly
`window_size * average_in_flight_per_file`. For the medium-files
workload this is a few hundred MiB, comparable to the existing
receiver-side concurrent_delta footprint.

## 5. Benchmarking plan (not executed in this PR)

Two workloads, both runnable inside the `rsync-profile` podman
container per `CLAUDE.md`. The container has upstream rsync 3.4.1 and
oc-rsync-dev pre-built; the workspace bind-mount exposes the source
tree at `/workspace`.

### Workload A - 100K small files (~4 KiB each)

Goal: expose syscall + dispatch overhead. Expectation: serial baseline
already saturates per-file fixed cost
(`docs/audits/profiling-100k-files.md`). Fan-out should be **net
neutral or slightly negative** here: the rolling-hash CPU per file is
trivial, the reorder buffer adds dispatch overhead.

```
podman exec rsync-profile bash -c '
  cd /tmp && rm -rf src dst && mkdir src dst
  for i in $(seq 1 100000); do
    head -c 4096 /dev/urandom > src/f$i.bin
  done
  /usr/bin/time -v oc-rsync-dev -a src/ dst/ 2> dst-baseline.time
  rm -rf dst && mkdir dst
  /usr/bin/time -v OC_RSYNC_SENDER_PARALLEL=1 oc-rsync-dev -a src/ dst/ \
    2> dst-parallel.time
'
```

Metrics: `Elapsed (wall clock) time`, `Maximum resident set size`,
`Percent of CPU this job got`. The CPU% column is the headline: the
parallel run should exceed 100 % iff the fan-out is doing work.

### Workload B - 1K medium files (~1 MiB each)

Goal: expose checksum CPU saturation. Each file has ~256 blocks of
4 KiB; per-file rolling sweep is non-trivial. Expectation: fan-out
should produce a measurable wall-time reduction roughly proportional
to `min(W, file_count / 1)` up to the reorder window cap.

```
podman exec rsync-profile bash -c '
  cd /tmp && rm -rf src dst && mkdir src dst
  for i in $(seq 1 1000); do
    head -c 1048576 /dev/urandom > src/f$i.bin
  done
  /usr/bin/time -v oc-rsync-dev -a src/ dst/ 2> dst-baseline.time
  rm -rf dst && mkdir dst
  /usr/bin/time -v OC_RSYNC_SENDER_PARALLEL=1 oc-rsync-dev -a src/ dst/ \
    2> dst-parallel.time
'
```

Same metrics. Add `perf stat -e cycles,instructions,cache-misses`
runs on each side if `perf` is available in the container.

### Comparison harness

`scripts/benchmark_hyperfine.sh` already wraps hyperfine for matched
pairs of binaries; extend it (in a follow-up) with a `--sender-mode
serial|parallel` flag once the prototype lands behind a feature flag.
Do not run this benchmark on master; the flag must be off by default.

## 6. Risks and open questions

- **`--checksum` interaction.** `-c` adds a whole-file pre-pass on
  the sender (`crates/transfer/src/generator/file_list/...`). That
  pre-pass already runs serially on the sender thread. Question: do
  we want to fold whole-file pre-checksum into the same fan-out, or
  keep it on a separate stage upstream of the request loop? Probably
  the latter: pre-checksum is a different lifecycle (file-list
  build), the rolling-hash fan-out lives inside the request loop.
- **`--inplace`.** Per `target/interop/upstream-src/rsync-3.4.1/sender.c:331-332`
  `updating_basis_file` is set when basis == destination. The sender
  reads from a file the receiver may also be writing. Today this is
  serial; with fan-out, multiple workers could be reading the same
  inode if the sender and the receiver share the disk (local
  transfer). The `match.c:208-215` pruning of `s->sums[i]` entries
  with `offset < offset` is per-file state and stays per-worker, so
  no cross-worker hazard, but we need to ensure the `--inplace` open
  uses `O_RDONLY` (it does:
  `crates/transfer/src/generator/transfer.rs` opens via
  `open_source_reader`).
- **`--append`.** Append mode (`sender.c:372-391`) takes a different
  `match_sums()` early path that just hashes the prefix and emits
  zero matches. It is faster than the regular path but still per-
  file serial; fan-out helps here too. No new hazard.
- **INC_RECURSE.** Sub-list streaming (`sender.c:227,261
  send_extra_file_list`) interleaves new file-list segments with
  file transfers. The fan-out producer (NDX-read thread) must
  continue to drive `encode_and_send_segment`
  (`generator/transfer.rs:113-118`) on a serial path; segment
  emission is wire-positioned at the top of each loop iteration,
  before any file record. Fan-out only applies to the file-record
  body, not the segment headers.
- **Compression context.** `token_encoder` (zstd / zlib / lz4) is
  one context per session
  (`generator/transfer.rs:74-80`). Workers must produce uncompressed
  wire ops; the writer thread compresses on emit. Alternatively,
  per-worker encoders with a session-level reset hand-off; this
  changes wire bytes (different zstd block boundaries) and is **not
  wire-equivalent** to upstream. Reject that variant.
- **`MSG_NO_SEND`.** `target/interop/upstream-src/rsync-3.4.1/sender.c:367-368`
  sends `MSG_NO_SEND` on open failure (mirrored at
  `generator/transfer.rs:301`). With fan-out, an open failure inside
  a worker must be reported back to the writer thread as a result
  variant, not via direct multiplex write. The writer translates
  the variant into the corresponding `MSG_NO_SEND` after honouring
  the reorder buffer's NDX position.
- **Determinism for batch mode.** `--write-batch` (`f_xfer = batch_fd`)
  and `--read-batch` consume / replay the wire byte stream. The
  fan-out reorders nothing on the wire (the reorder buffer guarantees
  identical wire bytes); batch files remain golden.
- **Audit invariants from `concurrent_delta/mod.rs:52-166`.** Adding
  a sender-side `par_iter` site means extending the SAFE / GUARDED /
  RISK matrix. The new site is GUARDED by `BoundedReorderBuffer`,
  same as the existing receiver site.

## 7. Distinct from sibling tasks

| Task | Scope | Where work happens | This audit |
|------|-------|--------------------|------------|
| #1023 | Intra-file rolling-hash parallelism for one huge file | Sender, inside `match_sums()` for one file | Different |
| #1079 | Multi-file delta-apply pipeline on the receiver | Receiver, post-wire, in `engine::concurrent_delta` | Different |
| #1024 | Parallel signature generation on the receiver | Receiver, basis-file signature build (completed) | Adjacent, mirrored on receiver only |
| #1325 | Concurrent delta consumer with reorder buffer | Receiver, after `recv_files()` byte read | Reuses the pattern |
| #1883 / #1884 | Head-of-line blocking on receiver-side concurrent pipeline | Receiver | Symmetric concern, called out in 4.3 |
| **#2048 (this audit)** | **Multi-file rolling-hash + strong-sum fan-out across files on the sender** | **Sender, inside the NDX-driven request loop** | **Investigation only** |

#1023 and #2048 are orthogonal: #1023 splits one file across workers,
#2048 splits many files across workers. The two could compose, but
that composition is out of scope here. #1079 is the receiver mirror of
#2048; the symmetry is intentional (`concurrent_delta/mod.rs` already
implements it on the receiver side). #2048 brings the same shape to
the sender's match path.

## 8. Recommendation

**Further investigate, then prototype behind a feature flag.** Land
this audit; do not change wire-emitting code on master. Next steps,
in order:

1. Wire benchmarks. The benchmark harness in section 5 must run
   *before* any code change, with the current serial baseline and
   with upstream rsync 3.4.1, on the `rsync-profile` container, on
   both Workload A and Workload B. Capture wall time, RSS, CPU%.
   Without this baseline we cannot validate that fan-out is a net
   win on Workload B and a non-regression on Workload A.
2. Prototype on a branch, behind `OC_RSYNC_SENDER_PARALLEL=1` env
   var (mirror the existing `OC_RSYNC_BUFFER_POOL_STATS` opt-in
   pattern at `crates/engine/src/local_copy/buffer_pool/pool.rs`). The
   prototype reuses `BoundedReorderBuffer`, `WorkQueue`, and
   `drain_parallel`; no new infra. Single-call site change in
   `crates/transfer/src/generator/transfer.rs:run_transfer_loop`.
3. Re-run benchmarks on the prototype. If Workload B wins by >= 1.5x
   wall time and Workload A regresses by <= 5 %, promote to default-
   on for non-`--inplace` non-`--append` transfers and gate large
   files (>= 1 GiB) to the serial path. Otherwise shelve and update
   this audit.
4. Extend the audit table in
   `crates/engine/src/concurrent_delta/mod.rs:52-166` to mark the
   new sender site as GUARDED by `BoundedReorderBuffer`.

The work is bounded, the wire format is untouched, and the building
blocks already exist. The risk is that the medium-files win is small
in absolute terms because real-world SSH transfers are network-bound,
not CPU-bound, on most workloads; the benchmark gate is therefore
load-bearing.
