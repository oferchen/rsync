# PIP-1 - Production token_loop vs ParallelDeltaApplier scaffold migration surface

Date: 2026-05-21
Scope: read-only research for PIP-2 (design) and PIP-3 (surface estimation)
Defers: benchmark sequencing -> PIP-6

## Goal

Map the production receive-side delta apply path (`token_loop` over an SPSC
pipe) against the feature-gated parallel scaffold (`ParallelDeltaApplier`
plus `ChunkBuilder`) so the PIP-2 migration design can target a concrete
seam. The audit walks both code paths, lists every datum that crosses a
thread boundary, and catalogs the back-pressure, ordering, and error
propagation invariants each path enforces.

The starting point is the project memory note
`project_parallel_interop_parity_gap.md`: the parallel scaffolding (BR-3i
verify, BR-3j DashMap, BR-3i.d ChunkBuilder adapter) is in place but the
production token loop still ships bytes through `FileMessage::Chunk(buf)`,
so the parallel applier is never exercised end-to-end. PIP-1 closes the
research gap before PIP-2 commits to a migration shape.

## 1. Current production path

### 1.1 Wire-message arrival

The production receiver streams one file at a time. The entry point that
handles a delta-bearing response is
`crates/transfer/src/transfer_ops/streaming.rs:74` -
`process_file_response_streaming`. It:

- Reads the wire header (basis path, target size, append offset, xattr
  values) at `streaming.rs:87` via `read_response_header`.
- Moves the per-file `ChecksumVerifier` from the network thread to the
  disk thread by `std::mem::replace` at `streaming.rs:103-106`, so all
  hashing happens off the network-critical path.
- Builds a `Box<BeginMessage>` carrying `file_path`, `target_size`,
  `file_entry_index`, the moved `ChecksumVerifier`, the device/inplace
  flags, the append offset, and the resolved xattr list at
  `streaming.rs:110-120`.
- Opens an optional `MapFile` for the basis at `streaming.rs:122-128` so
  COPY tokens can be resolved without going back to the kernel.
- Peeks the first delta token at `streaming.rs:138` to support the
  single-chunk coalescing optimisation (`WholeFile` message). If the
  first token is literal and the second is `DeltaToken::End` and the
  basis map is absent, the receiver fuses Begin + Chunk + Commit into one
  `FileMessage::WholeFile` send at `streaming.rs:156-163` and returns
  immediately.
- Otherwise it sends `FileMessage::Begin(begin_msg)` at
  `streaming.rs:176` and falls into `process_remaining_tokens` at
  `streaming.rs:187-198` or `:206-217` depending on whether the first
  token already produced a chunk.

`process_remaining_tokens` lives at
`crates/transfer/src/transfer_ops/token_loop.rs:77`. The per-token loop
(`token_loop.rs:97-203`) decodes one `DeltaToken` per iteration via
`token_reader.read_token(reader)` at `:100` and dispatches on the
variant:

- `DeltaToken::Literal` (`token_loop.rs:133-151`): consumes the literal
  payload through `literal_to_buf` (`token_loop.rs:51`), which either
  reuses a returned buffer or allocates a new `Vec<u8>` and reads exactly
  `len` bytes from the network. The resulting `Vec<u8>` is shipped via
  `file_tx.send(FileMessage::Chunk(buf))` at `token_loop.rs:143`.
- `DeltaToken::BlockRef(block_idx)` (`token_loop.rs:152-201`):
  bounds-checks the block index against the signature layout, computes
  the basis offset and byte length, calls `basis_map.map_ptr(...)` at
  `:179` to resolve the basis bytes, replays the bytes through the token
  reader for zlib dictionary parity (`token_reader.see_token(block_data)`
  at `:183`), copies the bytes into a recycled buffer, and ships
  `FileMessage::Chunk(buf)` at `token_loop.rs:187`.
- `DeltaToken::End` (`token_loop.rs:110-132`): reads the expected
  whole-file digest off the wire into a stack `[u8; MAX_DIGEST_LEN]`
  buffer, sends `FileMessage::Commit` at `token_loop.rs:118`, and
  returns a `StreamingResult` (`streaming.rs:28`) with
  `total_bytes`, `literal_bytes`, `matched_bytes`,
  `expected_checksum`, and `checksum_len`.

Buffer recycling is the load-bearing optimisation here. `recycle_or_alloc`
at `token_loop.rs:31` drains the `buf_return_rx` SPSC channel for
returned buffers, growing them in place when capacity is short. This
mirrors upstream rsync's static `simple_recv_token` buffer
(`token.c:284`) and is what keeps allocator pressure off the network
thread.

### 1.2 SPSC channel boundary

The channel that crosses the network -> disk thread boundary is the
lock-free SPSC ring at `crates/transfer/src/pipeline/spsc.rs:150`,
backed by `crossbeam_queue::ArrayQueue` plus an `AtomicBool` liveness
flag for each side. The producer spin-waits on `push` failure
(`spsc.rs:74-87`); the consumer spin-waits on `pop` failure
(`spsc.rs:110-121`). There are zero syscalls in the steady state, which
is why the receiver can afford a 128-slot ring (`DEFAULT_CHANNEL_CAPACITY`
at `crates/transfer/src/disk_commit/config.rs:32`) and still keep both
sides hot.

Three channels are constructed per transfer in `spawn_disk_thread` at
`crates/transfer/src/disk_commit/thread.rs:47-64`:

- `file_tx`/`file_rx` carrying `FileMessage` items, capacity
  `effective_channel_capacity()` (default 128).
- `result_tx`/`result_rx` carrying `io::Result<CommitResult>`, capacity
  `2 * effective_channel_capacity()`.
- `buf_return_tx`/`buf_return_rx` carrying returned `Vec<u8>` buffers,
  capacity `2 * effective_channel_capacity()`.

### 1.3 Data structures crossing the SPSC pipe

The enum at the boundary is `FileMessage` at
`crates/transfer/src/pipeline/messages.rs:21-45`:

| Variant | Payload | Producer site | Consumer site |
|---------|---------|---------------|---------------|
| `Begin(Box<BeginMessage>)` | per-file open metadata (see below) | `streaming.rs:176,202` | `thread.rs:195` (dispatches into `process_file`) |
| `Chunk(Vec<u8>)` | one literal or basis-resolved span | `token_loop.rs:143,187` | `process.rs:75` |
| `Commit` | end-of-file marker (no payload) | `token_loop.rs:118` | `process.rs:92` |
| `WholeFile { begin, data }` | coalesced single-chunk file | `streaming.rs:156` | `thread.rs:209` (dispatches into `process_whole_file`) |
| `Abort { reason: String }` | per-file abort with diagnostic | `token_loop.rs:90,103,114,137,161,189,198` | `process.rs:119` |
| `Shutdown` | terminate disk thread | shutdown sequence | `thread.rs:194` and `process.rs:124` |

`BeginMessage` (`messages.rs:51-99`) is the per-file open-time
descriptor. It carries `file_path: PathBuf`, `target_size: u64`,
`file_entry_index: usize`, `checksum_verifier: Option<ChecksumVerifier>`
(moved off the network thread), `is_device_target: bool`,
`is_inplace: bool`, `append_offset: u64`, and
`xattr_list: Option<XattrList>`. Anything that changes per-transfer
(temp dir, fsync, sparse policy, file list, ACL cache, io_uring depth,
IOCP policy) lives in `DiskCommitConfig` at
`crates/transfer/src/disk_commit/config.rs:42-97` and is borrowed by the
disk thread rather than cloned per file.

`CommitResult` (`messages.rs:110-119`) returns through `result_rx` after
each file completes: `bytes_written: u64`,
`file_entry_index: usize`, `metadata_error: Option<(PathBuf, String)>`,
and `computed_checksum: Option<ComputedChecksum>` (digest bytes plus
length, computed on the disk thread - see `process.rs:78-80`).

Note carefully: `DeltaWork` and `DeltaResult` from
`crates/engine/src/concurrent_delta/types.rs` are **not** part of this
hot path. They live in the `delta_pipeline` abstraction at
`crates/transfer/src/delta_pipeline/mod.rs:68-94` and the production
receiver instantiates the **sequential** implementation
(`SequentialDeltaPipeline` at `sequential.rs:27`). The work item carries
no chunk data - it is a per-file descriptor used only by the
`DeltaStrategy::process` dispatch in
`crates/engine/src/concurrent_delta/strategy/`. The pipeline that
actually moves bytes across threads on the production path is the
`FileMessage` SPSC channel above.

### 1.4 Disk thread consumer

`disk_thread_main` at
`crates/transfer/src/disk_commit/thread.rs:172-234` runs the consumer
loop. On Linux it creates one `IoUringDiskBatch`
(`thread.rs:179`), on Windows it falls through to `IocpDiskBatch`
(`thread.rs:183`), otherwise it uses the buffered writer with the
shared 256 KB scratch buffer at `thread.rs:178` (matching upstream's
static `wf_writeBuf`, `fileio.c:161`). Per-message dispatch:

- `FileMessage::Begin(begin)` -> `process_file` at
  `crates/transfer/src/disk_commit/process.rs:32`. Opens the destination
  via `open_output_file` (`process.rs:227`), constructs the appropriate
  writer (`make_writer` at `:277`), takes the moved `ChecksumVerifier`
  off the begin message (`process.rs:62`), and enters the inner per-chunk
  loop at `process.rs:66-141`:
  - `Chunk(data)`: updates the checksum, writes via the sparse-aware path
    or the batched writer at `process.rs:78-87`, increments
    `bytes_written`, and returns the buffer through `buf_return_tx.send`
    at `process.rs:90`.
  - `Commit`: drains sparse state, flushes and syncs, calls `commit_file`
    (`process.rs:100`), applies post-commit metadata
    (`apply_post_commit_metadata`), finalises the checksum, and returns
    `CommitResult`.
  - `Abort { reason }`: drops the output and the temp guard so the
    partial file is unlinked, returns `io::Error::other(reason)`.
  - `Shutdown`, `Begin`, `WholeFile`: invariant-violation errors
    (`process.rs:124-139`).
- `FileMessage::WholeFile { begin, data }` -> `process_whole_file` at
  `process.rs:150`, the coalesced one-shot variant.

### 1.5 Back-pressure, ordering, error propagation

**Back-pressure.**

- `Sender::send` spin-waits when the ring is full and surfaces
  `SendError` only when the receiver has dropped (`spsc.rs:74-87`). The
  effective fan-in to the disk thread is therefore the
  `channel_capacity` slot count (default 128). This bounds peak memory
  in the channel to roughly `channel_capacity * average chunk size`
  (~4 MB at 32 KB chunks per the comment at `config.rs:31`).
- The buffer return channel is sized at `2 *
  effective_channel_capacity()` (`thread.rs:51`) so the network thread
  never spin-waits on `recycle_or_alloc` even when the disk thread runs
  ahead.
- `try_recv` in `recycle_or_alloc` at `token_loop.rs:35` is
  non-blocking; on empty it falls back to a fresh allocation rather than
  stalling the producer.

**Ordering.**

- The production path is single-threaded per file at every stage. The
  network thread reads tokens sequentially via `token_reader.read_token`
  at `token_loop.rs:100`; the SPSC ring is single-producer
  single-consumer; the disk thread processes one file at a time
  (`thread.rs:192-222`).
- Cross-file ordering follows the file-list order the sender emits.
  There is no reorder buffer, sequence number, or `ReorderBuffer` on
  this path because the channel and the consumer both preserve
  submission order by construction.

**Error propagation.**

- Network read failures (`token_loop.rs:101-105`,
  `:134-140`) and basis bounds failures (`token_loop.rs:157-163`) both
  call the inline `send_abort` closure (`token_loop.rs:89-91`) which
  ships `FileMessage::Abort { reason }` to the disk thread *before*
  returning the `io::Error` to the caller. This lets the disk thread
  unwind the temp file via `TempFileGuard` (`process.rs:120-122`) even
  when the network thread is the one that observed the error first.
- A failed `file_tx.send(...)` is mapped to
  `io::ErrorKind::BrokenPipe` at `token_loop.rs:119-122,143-148,187-192`
  and at `streaming.rs:160-163,176-178,179-184,202-204`, propagating
  disk-thread disconnects back to the network thread.
- Disk-side failures land in `CommitResult` via `result_tx` for the
  caller to inspect; `metadata_error` is non-fatal and recorded
  separately so post-commit metadata application can degrade gracefully.
- Invariant violations on the disk thread (`Chunk` without `Begin`,
  `Begin` while another file is in flight, `Shutdown` mid-file) return
  `io::Error` variants at `thread.rs:223-230` and `process.rs:124-139`.

## 2. Parallel scaffold path

### 2.1 Module shape and gating

The parallel applier lives in
`crates/engine/src/concurrent_delta/parallel_apply.rs` and is
re-exported under the `parallel-receive-delta` feature at
`crates/engine/src/concurrent_delta/mod.rs:177-189`. The transfer-side
adapter `ChunkBuilder` lives at
`crates/transfer/src/delta_pipeline/chunk_builder.rs` and is gated by
the matching feature in `crates/transfer/Cargo.toml:107`. Neither file
is compiled into the default-built binary's hot path; the receiver
swap point is `enable_parallel_receive_delta` at
`crates/transfer/src/receiver/mod.rs:387-392`, which substitutes a
`ParallelDeltaPipeline` (operating on `DeltaWork`/`DeltaResult`, *not*
`DeltaChunk`) for the sequential pipeline. The
`ParallelDeltaApplier` itself has no production caller today; the only
non-test wiring is the `chunk_builder` adapter and its own unit tests.

### 2.2 DeltaChunk shape

`DeltaChunk` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:151-183` is the
unit of work that flows through the parallel applier:

- `ndx: FileNdx` (`parallel_apply.rs:153`) - file routing key.
- `chunk_sequence: u64` (`parallel_apply.rs:159`) - per-file monotonic
  submission counter assigned at builder time.
- `data: Vec<u8>` (`parallel_apply.rs:161`) - already-resolved bytes
  (literal payload or basis-mapped block).
- `is_literal: bool` (`parallel_apply.rs:166`) - discriminator kept for
  future stats split.
- `expected_strong: Option<ChecksumDigest>` (`parallel_apply.rs:182`) -
  per-chunk expected strong digest. Populated only for `matched`
  chunks; literal chunks always carry `None`.

Construction helpers: `DeltaChunk::literal(...)` at
`parallel_apply.rs:188` and `DeltaChunk::matched(...)` at
`parallel_apply.rs:200`, with the optional
`with_expected_strong(digest)` builder at `parallel_apply.rs:219`.

### 2.3 ChunkBuilder construction

`ChunkBuilder` at
`crates/transfer/src/delta_pipeline/chunk_builder.rs:104-227` is the
adapter PR #4646 (BR-3i.d) landed to turn wire delta tokens into
`DeltaChunk` values:

- Borrows the file's `FileSignature` for the duration of the apply
  (`chunk_builder.rs:107`), avoiding any clone of the per-block strong
  digests.
- Maintains a per-file `next_sequence: u64` counter
  (`chunk_builder.rs:108`) bumped once per produced chunk.
- `literal_chunk(data)` at `chunk_builder.rs:147` builds a literal
  chunk with `expected_strong = None`.
- `matched_chunk(block_index, basis_bytes)` at `chunk_builder.rs:175`
  looks up the block in `signature.blocks()`, bounds-checks the index
  (typed error `ChunkBuilderError::BlockIndexOutOfBounds` at
  `chunk_builder.rs:64-69`), len-checks the supplied basis bytes
  (`ChunkBuilderError::BasisLenMismatch` at
  `chunk_builder.rs:79-86`), and stamps the signature's stored strong
  digest into the chunk via `with_expected_strong(...)` at
  `chunk_builder.rs:194-196`.
- `next_chunk(token)` at `chunk_builder.rs:215` dispatches on
  `TokenForBuild` (`chunk_builder.rs:237-251`) - `Literal(Vec<u8>)`,
  `BlockRef { index, basis_bytes }`, or `End`.

The builder is a pure function over already-resolved bytes: it never
touches the network reader or the basis `MapFile`. That keeps the I/O
ownership exactly where it already lives in `token_loop` and is the
seam PIP-2 will exploit.

### 2.4 register / verify / write / finish lifecycle

The applier itself is `ParallelDeltaApplier` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:305-338`:

- **register**: `register_file(ndx, writer)` at
  `parallel_apply.rs:428-452` builds the per-file `FileSlot`
  (`parallel_apply.rs:227-278`, holding the `Box<dyn Write + Send>`
  writer, the per-file `ReorderBuffer<DeltaChunk>` sized by
  `DEFAULT_PER_FILE_REORDER_CAPACITY = 64` at
  `parallel_apply.rs:355`, and a `bytes_written` counter), then inserts
  it into the `DashMap<FileNdx, Arc<Mutex<FileSlot>>>` shard at
  `parallel_apply.rs:443-451`. Double-registration is rejected.
- **verify**: `apply_chunk_parallel(chunk)` at
  `parallel_apply.rs:468-485` clones the per-file `Arc` via
  `slot_for(ndx)` (`parallel_apply.rs:586-595`, which drops the shard
  guard at the end of the expression), schedules
  `Self::verify_chunk(strategy, chunk)` on `rayon::join` at
  `parallel_apply.rs:477`, and unwraps the `VerifiedChunk`. The verify
  step (`parallel_apply.rs:616-636`) computes the strong digest of
  `chunk.data` using the configured strategy (default MD5,
  `parallel_apply.rs:367-372`) and compares against `expected_strong`
  when present. A mismatch yields
  `ParallelApplyError::ChecksumMismatch` (`parallel_apply.rs:113-128`)
  which is mapped to `io::Error` at `parallel_apply.rs:131-139`.
- **write**: after verify, the caller takes the per-file mutex at
  `parallel_apply.rs:480-482` and calls `FileSlot::ingest(chunk)` at
  `parallel_apply.rs:248-258`, which inserts the chunk into the
  per-file `ReorderBuffer` keyed by `chunk_sequence` and drains every
  contiguous run via `drain_ready()` straight into
  `FileSlot::write_chunk` at `parallel_apply.rs:260-267`. The writer
  only ever sees chunks in strict `chunk_sequence` order.
- **batch verify**: `apply_batch_parallel(chunks)` at
  `parallel_apply.rs:499-526` shards the verify step across rayon via
  `into_par_iter().with_min_len(min_len)` and collects
  `Result<Vec<VerifiedChunk>, ParallelApplyError>`. Rayon's parallel
  collect short-circuits on the first error
  (`parallel_apply.rs:511-516`). On success the writes are issued
  serially in completion order at `parallel_apply.rs:518-524`; the
  per-file reorder buffer still re-establishes per-file byte order
  before each chunk hits the writer.
- **finish**: `finish_file(ndx)` at `parallel_apply.rs:555-584`
  removes the shard entry, `Arc::try_unwrap`s the slot (typed
  `ApplierStillReferenced` error if any clones leaked,
  `parallel_apply.rs:73-84`), drains the inner mutex (typed
  `SlotPoisoned` at `:88-94`), and rejects un-drained chunks
  (`UndrainedChunks` at `:99-109`) before handing the `Box<dyn Write +
  Send>` writer back to the caller for its own finalisation step.

### 2.5 Strong-checksum strategy plumbing

`ParallelDeltaApplier::with_strategy(concurrency, strategy)` at
`parallel_apply.rs:382-389` is the constructor the receiver pipeline
will use to thread the negotiated `ChecksumStrategy` in. The strategy
is held behind `Arc<dyn ChecksumStrategy>` so each rayon worker clones
the handle cheaply (`parallel_apply.rs:476`). The default constructor
falls back to MD5 with seed 0 (`parallel_apply.rs:367-372`), matching
the protocol >= 30 fallback that
`crates/transfer/src/shared/checksum.rs::ChecksumFactory::from_negotiation`
resolves when no `NegotiationResult` is present.

## 3. Gap analysis

### 3.1 Message-shape differences

| Aspect | Production (`token_loop`) | Scaffold (`ParallelDeltaApplier`) |
|--------|---------------------------|-----------------------------------|
| Unit of work | `FileMessage::Chunk(Vec<u8>)` plus `Begin`/`Commit`/`Abort` framing | `DeltaChunk` (carrying `ndx`, `chunk_sequence`, `data`, `is_literal`, `expected_strong`) |
| File routing | Implicit - one file in flight per disk thread; `Begin` opens, `Commit` closes | Explicit - `FileNdx` routes to a registered slot in a `DashMap` |
| Per-chunk sequence | None - SPSC preserves order by construction | Required - `chunk_sequence: u64` assigned by `ChunkBuilder::next_sequence` |
| Per-chunk verify metadata | None - the disk thread updates a single `ChecksumVerifier` per file | `Option<ChecksumDigest>` populated only for basis-match chunks |
| Per-file metadata | `BeginMessage` (path, size, file_entry_index, checksum verifier, device/inplace/append, xattrs) | `register_file(ndx, Box<dyn Write + Send>)` only - everything else is the caller's problem |
| End-of-file signal | `FileMessage::Commit` + 16-byte expected whole-file digest read by the network thread | `finish_file(ndx)` returns the writer back; whole-file digest is not part of the chunk shape |
| Error frame | `FileMessage::Abort { reason: String }` inline in the channel | `io::Error` returned synchronously from `apply_chunk_parallel`/`apply_batch_parallel`; per-chunk mismatch carries typed `ParallelApplyError::ChecksumMismatch` |

The chunk shape difference is not blocking: `ChunkBuilder` already
adapts every `DeltaToken` variant the production path emits. The
missing piece is the lifecycle: the parallel applier has no
`Begin`/`Commit` equivalent because `register_file` and `finish_file`
are synchronous on the caller's thread and run outside the chunk
stream. PIP-2 will need to decide where to hang the equivalent of the
`BeginMessage` payload (device/inplace flags, append offset, xattr
list, file_entry_index, target_size, checksum verifier).

### 3.2 Back-pressure mechanisms

| Path | Mechanism | Bound | Failure mode |
|------|-----------|-------|--------------|
| Production | SPSC `ArrayQueue` spin-wait on `push` (`spsc.rs:79-86`) | `channel_capacity` slots (default 128) | Spin-wait when full; `SendError` only on consumer drop |
| Production buffer return | SPSC `ArrayQueue` non-blocking `try_recv` (`token_loop.rs:35`) | `2 * channel_capacity` slots | Falls back to fresh `Vec::with_capacity` on empty |
| Scaffold per-chunk | Synchronous - `apply_chunk_parallel` runs verify and write inline on the caller's thread | None on submissions; per-file reorder buffer caps at 64 entries by default | Caller blocks on `rayon::join` plus per-file mutex; `ReorderBuffer` full returns `io::Error::other("parallel apply reorder full: ...")` at `parallel_apply.rs:248-258` |
| Scaffold per-batch | `apply_batch_parallel` consumes the whole `Vec<DeltaChunk>` (`parallel_apply.rs:499-526`); rayon shards by `min_len = total.div_ceil(cap.max(1)).max(1)` | Caller must size the batch; no built-in fairness | Holds all chunks in memory until `collect` finishes |

The production path has explicit back-pressure that propagates
naturally to the network thread (network spin-waits on the SPSC ring).
The scaffold relies on the caller to throttle submissions; the only
hard bound is the per-file `ReorderBuffer` capacity at
`DEFAULT_PER_FILE_REORDER_CAPACITY = 64` (`parallel_apply.rs:355`),
which errors rather than blocking when overrun
(`parallel_apply.rs:248-258`). This is the same behaviour flagged in
`project_reorder_capacity_hard_default.md`.

### 3.3 Ordering invariants

| Invariant | Production | Scaffold |
|-----------|------------|----------|
| Per-file byte order | Preserved structurally: single producer, single consumer, single in-flight file | Preserved via per-file `ReorderBuffer<DeltaChunk>` keyed by `chunk_sequence`, drained on every `ingest` (`parallel_apply.rs:248-258`); writer sits behind a per-file `Mutex` |
| Cross-file order | File-list order by construction (sender emits one file at a time, disk thread processes one file at a time) | Not guaranteed - independent files complete independently; the caller is responsible for whatever cross-file ordering the wire output needs (the existing `DeltaConsumer` + `ReorderBuffer` covers the `DeltaWork`/`DeltaResult` pipeline but is separate from `DeltaChunk` ingestion) |
| Whole-file digest verification | Disk thread accumulates the digest as chunks arrive, compares against the wire-supplied expected digest in `finalize_checksum` after `Commit` | Not in scope - per-chunk verify against `expected_strong` only; whole-file digest is the caller's problem after `finish_file` returns the writer |

Cross-file ordering is the structural gap. The production receiver
treats the wire as a serial file stream and never has more than one
file open; the parallel applier exposes a `DashMap`-keyed shape but the
chunk-stream-to-DashMap mapping has no `Begin` equivalent today. PIP-2
will need to define when `register_file` happens relative to the wire
header arrival, and how the equivalent of `BeginMessage` metadata is
threaded through.

### 3.4 Error propagation

| Failure | Production | Scaffold |
|---------|------------|----------|
| Network read mid-chunk | `send_abort(file_tx, "network read error: ...")` (`token_loop.rs:90,103,114,137`) then return `io::Error` | No analogue - the caller would have to construct an error path; the applier has no `Abort` channel |
| Basis bounds violation | `send_abort(...)` + `Err(io::Error::new(InvalidData, ...))` at `token_loop.rs:158-163` | Typed `ChunkBuilderError::BlockIndexOutOfBounds` at `chunk_builder.rs:64-69` (builder-side) |
| Per-chunk checksum mismatch | Whole-file digest only, evaluated after `Commit` on the disk thread | Per-chunk - `ParallelApplyError::ChecksumMismatch` (`parallel_apply.rs:113-128`) on `apply_chunk_parallel`, short-circuits `apply_batch_parallel` |
| Disk-thread disconnect | `file_tx.send(...)` returns `Err`, mapped to `io::ErrorKind::BrokenPipe` (`token_loop.rs:119-122,143-148,187-192`) | Per-file slot poisoning - typed `ParallelApplyError::SlotPoisoned` at `parallel_apply.rs:88-94` |
| Writer failure | Disk thread surfaces via `CommitResult` through `result_tx` | Synchronous on the caller thread; `io::Error` from `Write::write_all` propagates out of `apply_chunk_parallel` |

The notable scaffold improvement is per-chunk verification: the
production path can only detect corruption at end-of-file, whereas the
parallel path can refuse a basis-match chunk before its bytes reach the
writer. The notable scaffold gap is the missing `Abort` shape - the
production receiver explicitly notifies the disk thread on network
errors so the temp file is unlinked synchronously. The migration will
have to decide whether the applier grows an `abort_file(ndx, reason)`
entry point or whether the caller drives unlinking via a dropped
writer after a failure.

## 4. Migration shape sketch (advisory, for PIP-2)

Bullet sketch only - design choices belong in PIP-2.

- **Keep `BeginMessage` as the per-file open descriptor.** The disk
  thread's `process_file` already builds the writer and registers the
  checksum verifier on open. The cleanest mapping is to have the disk
  thread call `applier.register_file(ndx, writer)` after `open_output_file`
  and keep `BeginMessage` flowing on its current channel.
- **Replace `FileMessage::Chunk(Vec<u8>)` with `FileMessage::Chunk { ndx, chunk }`
  where `chunk: DeltaChunk`.** The producer side becomes
  `chunk_builder.literal_chunk(buf)` or `chunk_builder.matched_chunk(idx, bytes)`
  at the existing `token_loop.rs:143,187` send sites. Per-file sequence
  numbers come for free from `ChunkBuilder::next_sequence`.
- **Keep the SPSC channel as the network -> applier-driver boundary.**
  Do not push individual chunks through `rayon::join` from the network
  thread; the spin-wait ring already preserves the back-pressure the
  receiver needs. The applier-driver thread (currently the disk
  thread) drains the channel and calls `apply_chunk_parallel` or
  `apply_batch_parallel`. This keeps the wire-side hot path
  single-producer and avoids the `rayon::join` per-chunk noop the
  memory note flags (`project_rayon_join_per_chunk_noop.md`).
- **Decide where the whole-file checksum lives.** Two viable shapes:
  (a) keep `ChecksumVerifier` on the disk thread, hashing each
  `chunk.data` after `apply_chunk_parallel` returns successfully; or
  (b) hash inside `FileSlot::write_chunk` so the verifier sees bytes
  in `chunk_sequence` order. Option (a) preserves the current
  `CommitResult::computed_checksum` flow without changes; option (b)
  removes the duplicate accumulation but couples the applier to the
  per-file digest.
- **Decide on the `Abort` shape.** Either grow
  `ParallelDeltaApplier::abort_file(ndx, reason)` that drops the slot
  and returns the writer for synchronous cleanup, or rely on the
  caller dropping the writer after an `io::Error` from
  `apply_chunk_parallel`. The first preserves the current
  `FileMessage::Abort` semantics; the second is structurally simpler.
- **Bound the per-file reorder buffer.** The current
  `DEFAULT_PER_FILE_REORDER_CAPACITY = 64` errors rather than blocking
  on overflow (`parallel_apply.rs:248-258`). PIP-2 should either size
  this to the SPSC `channel_capacity` (default 128) or wire in the
  spillable variant `SpillableReorderBuffer` from
  `crates/engine/src/concurrent_delta/spill/`.
- **Defer the `WholeFile` coalescing decision.** The single-chunk
  optimisation at `streaming.rs:143-198` is a real perf win for
  small-file workloads. The migration can keep it (one
  `DeltaChunk::literal` with `chunk_sequence = 0`, followed
  immediately by `finish_file`) or drop it if measurement shows the
  reorder buffer overhead is in the noise.
- **Keep `DeltaWork` / `DeltaResult` out of the chunk path.** They
  belong to the per-file dispatch layer in `delta_pipeline/`. The
  migration is orthogonal: the chunk-stream lives below
  `submit_work(DeltaWork)`, not alongside it.

## 5. Surface impact for PIP-3

Files that will likely move under PIP-2 (worst-case if the chunk
boundary turns into the canonical receive shape):

### Production hot path (touch required)

- `crates/transfer/src/transfer_ops/token_loop.rs` (204 lines) - swap
  the `file_tx.send(FileMessage::Chunk(buf))` sites for
  `chunk_builder.next_chunk(...)`-produced `DeltaChunk` values.
- `crates/transfer/src/transfer_ops/streaming.rs` (220 lines) -
  rebuild the single-chunk coalescing path and the begin/commit
  framing around `DeltaChunk`.
- `crates/transfer/src/pipeline/messages.rs` (119 lines) - change the
  `FileMessage::Chunk` variant payload (or introduce a new
  `ChunkMsg` variant alongside it for the migration period).
- `crates/transfer/src/disk_commit/process.rs` (~640 lines, only the
  per-chunk dispatch at `:75-91` and the whole-file path at `:150-208`
  in scope) - become the applier driver: register on `Begin`, call
  `apply_chunk_parallel` on `Chunk`, `finish_file` on `Commit`.
- `crates/transfer/src/disk_commit/thread.rs` (234 lines) - own the
  `ParallelDeltaApplier` instance and pass it into `process_file` /
  `process_whole_file`.

### Already in place (no edit, just wire up)

- `crates/transfer/src/delta_pipeline/chunk_builder.rs` (538 lines,
  feature-gated) - the wire-token-to-chunk adapter and its tests.
- `crates/engine/src/concurrent_delta/parallel_apply.rs` (1133 lines,
  feature-gated) - the applier itself, plus per-file `FileSlot` and
  `ReorderBuffer<DeltaChunk>` plumbing.
- `crates/engine/src/concurrent_delta/types.rs::FileNdx` (used as the
  applier's routing key) and the existing `ReorderBuffer` /
  `SpillableReorderBuffer`.

### Feature-flag plumbing

- `crates/transfer/Cargo.toml:107` - `parallel-receive-delta` feature.
- `crates/engine/src/concurrent_delta/mod.rs:177-189` - re-exports.
- `crates/transfer/src/delta_pipeline/mod.rs:34,43` - feature gates on
  `chunk_builder` module and public re-exports.
- `crates/transfer/src/receiver/mod.rs:387-392` -
  `enable_parallel_receive_delta` swap-in site; PIP-2 will likely
  fold the chunk-path wiring through here.

### Tests likely affected

- `crates/transfer/src/transfer_ops/streaming.rs` tests (the
  single-chunk coalescing path).
- `crates/transfer/src/disk_commit/process.rs` per-chunk and
  whole-file tests.
- `crates/transfer/src/delta_pipeline/tests/` (existing pipeline
  tests, which exercise `SequentialDeltaPipeline` /
  `ParallelDeltaPipeline` over `DeltaWork`; will need a parallel
  chunk-path equivalent).
- `crates/engine/src/concurrent_delta/parallel_apply.rs::tests` -
  property tests already cover the per-file order invariant and can
  be reused.

### Headline counts

- ~5 production files modified (the four hot-path files plus
  `messages.rs`).
- ~3 feature-gate locations touched.
- Zero new crates; zero new public traits required (the
  `ReceiverDeltaPipeline` abstraction stays intact, the applier-driver
  swap happens inside the disk-commit thread).
- The migration is contained to the transfer crate's receiver side
  plus the engine `concurrent_delta` module that already houses the
  applier.

PIP-3 will refine these counts after PIP-2 picks the migration shape
(single-channel rewrite vs. parallel-applier-as-feature-flag with
both code paths coexisting during cutover).

## 6. References

- Code:
  - `crates/transfer/src/transfer_ops/token_loop.rs`
  - `crates/transfer/src/transfer_ops/streaming.rs`
  - `crates/transfer/src/pipeline/messages.rs`
  - `crates/transfer/src/pipeline/spsc.rs`
  - `crates/transfer/src/disk_commit/thread.rs`
  - `crates/transfer/src/disk_commit/process.rs`
  - `crates/transfer/src/disk_commit/config.rs`
  - `crates/transfer/src/delta_pipeline/mod.rs`
  - `crates/transfer/src/delta_pipeline/sequential.rs`
  - `crates/transfer/src/delta_pipeline/parallel.rs`
  - `crates/transfer/src/delta_pipeline/chunk_builder.rs`
  - `crates/transfer/src/receiver/mod.rs`
  - `crates/engine/src/concurrent_delta/mod.rs`
  - `crates/engine/src/concurrent_delta/parallel_apply.rs`
  - `crates/engine/src/concurrent_delta/types.rs`
- Prior audits:
  - `docs/audits/br-3i-a-verify-chunk-audit-2026-05-20.md`
  - `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md`
  - `docs/audits/arc-try-unwrap-classification.md`
- Upstream:
  - `token.c:284` `simple_recv_token` (the static-buffer pattern
    `recycle_or_alloc` mirrors)
  - `receiver.c:recv_files()` (the sequential per-file loop)
  - `receiver.c:receive_data()` (the per-token apply step)
