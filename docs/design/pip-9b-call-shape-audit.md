# PIP-9.b.1 - audit of the sequential `apply_delta_tokens` call shape

Date: 2026-05-22
Status: audit input for PIP-9.b (production wire-up). Read-only - no source
files change as part of this task.

This document fixes a precise contract for the single sequential call site
identified by the PIP-9 design (`docs/design/pip-9-parallel-receive-wireup.md`,
section 1.1) so that PIP-9.b can build a feature-gated parallel arm that is
byte-identical on the receiver side. The audit covers inputs, outputs, side
effects, the sequential algorithm, equivalence invariants, and the failure
modes that the PIP-9.b.2 sketch must defend against.

## 1. Call site under audit

File: `crates/transfer/src/receiver/transfer/sync.rs`, lines 241-253:

```
241            apply_delta_tokens(
242                reader,
243                &mut output,
244                &mut sparse_state,
245                &mut basis_map,
246                signature_opt.as_ref(),
247                &mut token_reader,
248                &mut token_buffer,
249                &mut checksum_verifier,
250                self.checksum_seed,
251                &file_path,
252                &mut total_bytes,
253            )?;
```

The callee is defined in the same file at lines 445-573. The call sits inside
the per-file loop of `ReceiverContext::run_sync` (sync.rs:98-398). Everything
upstream of the call performs NDX exchange, signature write-out, temp-file
open, and basis open; everything downstream performs sparse finish, fsync,
backup rename, temp-to-final rename, and metadata application. The call is
the only producer of destination-file bytes inside the loop.

## 2. Inputs

Eleven distinct arguments are passed in. For each: declared type, ownership
relative to the call, and the upstream-of-the-call site that built the value.

| # | Argument            | Type                                        | Ownership            | Built at sync.rs |
|---|---------------------|---------------------------------------------|----------------------|------------------|
| 1 | `reader`            | `&mut crate::reader::ServerReader<R>`       | mutable borrow       | line 49 (caller-supplied) |
| 2 | `output`            | `&mut BufWriter<File>`                      | mutable borrow       | line 214 (over temp file) |
| 3 | `sparse_state`      | `&mut Option<SparseWriteState>`             | mutable borrow       | lines 217-222 (gated on `--sparse`) |
| 4 | `basis_map`         | `&mut Option<MapFile>`                      | mutable borrow       | lines 224-237 (open of `basis_path_opt`) |
| 5 | `signature_opt`     | `Option<&engine::signature::FileSignature>` | shared borrow        | line 165 (`basis_result.signature`) |
| 6 | `token_reader`      | `&mut TokenReader`                          | mutable borrow       | line 94 (loop-invariant, reset line 239) |
| 7 | `token_buffer`      | `&mut TokenBuffer`                          | mutable borrow       | line 88 (loop-invariant scratch) |
| 8 | `checksum_verifier` | `&mut ChecksumVerifier`                     | mutable borrow       | lines 82-87 (loop-invariant, reset inside callee on End) |
| 9 | `checksum_seed`     | `i32` (Copy)                                | move-by-Copy         | `self.checksum_seed` (session-wide) |
| 10 | `file_path`         | `&Path`                                     | shared borrow        | lines 107-111 (per-file) |
| 11 | `total_bytes`       | `&mut u64`                                  | mutable borrow       | line 215 (per-file accumulator) |

Lifetime envelope: every borrow is scoped to the per-file iteration. The
session-scoped objects (`token_reader`, `token_buffer`, `checksum_verifier`)
outlive the call; they are reset/replaced at file boundaries either before
the call (`token_reader.reset()` at line 239) or by the callee on the `End`
token (verifier replacement, callee lines 467-470).

Important non-arguments (state the call reads/writes indirectly via
`reader.try_borrow_exact`): the reader's internal frame buffer is mutated by
zero-copy slice borrows (callee line 505).

## 3. Outputs

The function returns `io::Result<()>` (sync.rs:457).

- `Ok(())` - reached on the `End` token after a successful whole-file digest
  comparison (callee line 495). No payload data; all observable output is in
  the side effects below.
- `Err(io::Error)` - five distinct error productions, all in
  `apply_delta_tokens`:
  - reader-layer I/O errors propagate via `?` (read_token, read_exact,
    try_borrow_exact, MapFile::map_ptr): preserved kind.
  - `ErrorKind::InvalidData` when the receiver-computed digest length
    diverges from the negotiated length (callee lines 473-482).
  - `ErrorKind::InvalidData` when computed digest differs from sender's
    expected digest bytes (callee lines 483-494).
  - `ErrorKind::InvalidData` when a `BlockRef(idx)` arrives without a basis
    (callee lines 519-531).
  - `ErrorKind::InvalidData` when `idx >= block_count` (callee lines 536-545).

All `InvalidData` errors carry a formatted message including `file_path`,
`error_location!()`, and `role_trailer::receiver()`. The parallel arm must
emit the same trailers and same message shape so existing log assertions and
exit-code paths stay matched.

## 4. Side effects

Exhaustive list of state the call mutates, in the order it touches them:

1. **`reader`** - consumed bytes via `read_token`, `read_exact`,
   `try_borrow_exact`; possible internal-buffer state changes.
2. **`output`** - per-chunk `write_all` calls through `write_chunk`
   (sync.rs:577-588); when sparse is active, `Seek::seek` calls on the
   underlying file via `SparseWriteState::flush` skip zero runs.
3. **`sparse_state`** - `pending_zeros` accumulates / flushes as chunks
   arrive; finalisation happens after the call returns
   (`SparseWriteState::finish` at sync.rs:256).
4. **`basis_map`** - `map_ptr(offset, len)` faults pages; on Linux+io_uring
   the map may register/release pinned pages.
5. **`signature_opt`** - read-only.
6. **`token_reader`** - advances internal token stream; `see_token` records
   block-data into the deflate dictionary (callee line 567).
7. **`token_buffer`** - `resize_for(len)` grows the scratch buffer when a
   `Pending` literal does not fit in the reader's borrow window.
8. **`checksum_verifier`** - `update(data)` per literal/block-ref;
   `finalize_into` consumes the verifier; `for_algorithm_seeded` rebuilds it
   in place via `std::mem::replace` for the next file (callee lines 467-470).
9. **`total_bytes`** - incremented per literal length and per block size;
   used by the caller for `bytes_received` rollup (sync.rs:396).

No log emissions, no stats-struct writes outside `total_bytes`, no
verify-chain hand-off (whole-file verify happens inline on `End`), no
spawning of background work, no fsync, no rename. Every commit-side
operation (sparse finish, fsync, backup rename, atomic rename, metadata)
runs after the call returns.

## 5. Sequential algorithm sketch

The callee body (sync.rs:445-573) is a single `loop { match read_token(...) }`:

```
loop:
  tok = token_reader.read_token(reader)?
  match tok:
    End:
      n = checksum_verifier.digest_len()
      expected = read_exact(reader, n)
      old = mem::replace(checksum_verifier,
                          ChecksumVerifier::for_algorithm_seeded(algo, seed))
      computed = old.finalize_into(buf)
      if computed != expected: return InvalidData
      return Ok(())
    Literal(Ready(data)):
      write_chunk(output, sparse_state, &data)
      checksum_verifier.update(&data)
      *total_bytes += data.len()
    Literal(Pending(len)):
      data = reader.try_borrow_exact(len)?  -- zero-copy fast path
              .unwrap_or_else read_exact into token_buffer
      write_chunk(output, sparse_state, data)
      checksum_verifier.update(data)
      *total_bytes += len
    BlockRef(idx):
      (sig, map) = (signature_opt, basis_map.as_mut()) or InvalidData
      validate idx < layout.block_count()
      offset = idx * layout.block_length()
      len    = if last block { layout.remainder() or block_length }
                else { block_length }
      block_data = basis_map.map_ptr(offset, len)?
      write_chunk(output, sparse_state, block_data)
      checksum_verifier.update(block_data)
      token_reader.see_token(block_data)?  -- feed deflate dictionary
      *total_bytes += len
```

The loop is strictly serial: every chunk is written, hashed, and counted
before the next token is read. There is no concurrency, no reordering, no
look-ahead.

## 6. Parallel-arm equivalence contract

For PIP-9.b's parallel arm to be byte-identical on the receive side it
must preserve every invariant in this list. Numbered for citation in
PIP-9.b.2:

1. **Destination byte order**: bytes appearing at file offset `o` after the
   sequential call must appear at the same offset `o` after the parallel
   call. The token stream defines a total order (Literal/BlockRef sequence);
   the destination layout is that order concatenated. Parallel arms may
   reorder *reads* (basis page faults, decompression) but the *write*
   sequence to `output` must match.
2. **Verify chunk order vs writes**: the whole-file checksum is computed
   over the same byte sequence as the writes, in the same order. Per-chunk
   verify-chain dispatches (e.g. BR-3i.d's per-chunk strong digest) must
   feed the file-level verifier in destination-offset order, or be combined
   via an order-independent reducer that produces the same digest.
3. **Writer flush boundaries**: the sequential arm never flushes
   `BufWriter` from inside the call. The first flush is the caller's
   `BufWriter::into_inner` at sync.rs:271. The parallel arm must keep the
   same property: any per-chunk write goes through a writer that releases
   to disk at the same boundary, otherwise `fsync` semantics (sync.rs:278)
   and temp-rename atomicity diverge.
4. **Sparse-state ownership**: `SparseWriteState::pending_zeros` is the
   only persistent in-memory marker for an in-flight hole. After the call
   returns, the caller invokes `sparse.finish(&mut output)` (sync.rs:256)
   to seek-forward + tail-write. Any parallel arm must end with the same
   `pending_zeros` value as the sequential arm so the post-call finish
   produces the same final file size.
5. **Basis read order is *not* invariant**: pages may be faulted in any
   order. That is the point of the parallel arm. What is invariant is
   *which* pages are read and that each `map_ptr(offset, len)` returns
   the same bytes the sequential arm would have read.
6. **`see_token` semantics**: zstd/zlib token-reader state (callee line
   567) must observe block-data tokens in token-stream order, or the
   compression dictionary will diverge for subsequent tokens. The parallel
   arm cannot reorder `see_token` calls; either feed them sequentially on
   a dedicated thread or commit to non-compressed streams when going
   parallel.
7. **`total_bytes` final value**: must equal the sequential total. The
   sum is order-independent (commutative `+=`), so this is the cheapest
   invariant - but the *moment* of update relative to chunk completion
   matters for any future stats stream. Today no caller reads
   `total_bytes` until after the call returns, so order does not leak;
   PIP-9.b.2 should document that intermediate values are not observable.
8. **Verifier replacement at End**: the `for_algorithm_seeded(algo, seed)`
   rebuild (callee lines 467-470) must happen before returning `Ok`. The
   sequential arm guarantees this because End handling is on the same
   thread; the parallel arm must drain all in-flight chunks *before*
   replacing the verifier or the new file's prefix will be hashed by
   the old verifier.
9. **Error short-circuit**: the sequential arm stops reading tokens at
   the first `Err`. The parallel arm must guarantee that an
   `InvalidData` from chunk N is reported even if chunks N+1..M complete
   first; the destination file must not contain bytes from chunks past
   the failing one (or the caller's temp-rename will commit a corrupt
   file).
10. **Pre-call invariants preserved**: `signature_opt`/`basis_map` pair
    must remain pinned for the duration of any in-flight chunk; the
    sequential arm makes this trivial; the parallel arm must hold the
    `MapFile` alive until `flush_workers(ndx)` returns.

## 7. Pre-PIP-9.b risk catalog

Failure modes a naive parallel implementation can introduce. PIP-9.b.2's
dispatch sketch must call out each by number:

1. **Out-of-order writes**: dispatching Literal/BlockRef chunks to a
   thread pool without re-sequencing yields a destination file whose
   bytes are correct individually but in the wrong offset, producing a
   silent corruption (PIP-7 reproduced this exact symptom on file_1).
2. **Double-flush of BufWriter**: a per-chunk `flush()` inside the
   parallel arm undoes upstream's batched syscall pattern, raising
   `sendto`/`write` count per file and breaking the assumed atomicity
   window around temp-rename.
3. **Lost basis pin**: dropping `basis_map` while a worker still holds
   a slice from `map_ptr` segfaults on Linux (unmapped page) and reads
   stale bytes on Windows (file handle closed but mapping cached).
   The parallel arm must keep `MapFile` alive across the
   `flush_workers` barrier.
4. **`see_token` reorder**: feeding the deflate/zstd dictionary in
   dispatch order rather than token-stream order corrupts every
   subsequent literal in the file (zstd more aggressively than zlib).
   Visible as InvalidData on a later file in the same session.
5. **Sparse-state race**: two threads writing through the same
   `SparseWriteState` race on `pending_zeros`; one wins and the other's
   zero-run vanishes, producing a wrong-sized output. The
   `SparseWriteState` is `!Sync` for accumulation; the parallel arm
   must keep sparse handling on the commit thread or replace the
   state with a per-chunk position-aware variant.
6. **Verifier update reorder**: MD5/MD4/XXH3 over chunks A then B
   yields a different digest than B then A. Parallel arms must
   either feed `verifier.update(...)` strictly in destination-offset
   order or use a tree-hash reducer (which the protocol does not
   support).
7. **Verifier swap before drain**: replacing `checksum_verifier` on
   the End-token thread before workers finish their `update` calls
   leaves the next file's prefix hashed under the old verifier. The
   `flush_workers(ndx)` barrier must precede the swap.
8. **`total_bytes` torn read**: even though no caller currently
   reads it mid-flight, releasing it as a plain `&mut u64` to
   multiple workers is unsound (Rust aliasing). PIP-9.b.2 must
   pick either `AtomicU64` or per-worker shards reduced on drain.
9. **Reader-buffer slice escape**: `reader.try_borrow_exact(len)`
   returns a slice into the reader's frame buffer. Handing that
   slice to a worker that outlives the next `read_token` is UB
   even when wrapped in `Arc<[u8]>` (the borrow is non-`'static`).
   The parallel arm must copy or re-frame.
10. **Error swallow**: returning `Ok(())` from the parallel arm when
    a worker has produced `InvalidData` but the End-token thread
    has not observed it yet commits a corrupt temp file via the
    caller's rename. The drain-then-collect pattern in
    `ParallelDeltaApplier::finish_file` (RJN-3 era) handles this
    today; PIP-9.b.2 must continue to gate the post-call commit on
    a complete drain.

## 8. References

- `crates/transfer/src/receiver/transfer/sync.rs:241-253` - the call site.
- `crates/transfer/src/receiver/transfer/sync.rs:445-573` - the callee.
- `crates/transfer/src/receiver/transfer/sync.rs:577-588` - `write_chunk`.
- `crates/transfer/src/delta_apply/checksum.rs:12-188` - `ChecksumVerifier`
  enum + `for_algorithm_seeded` rebuild.
- `crates/transfer/src/delta_apply/sparse.rs` - `SparseWriteState`.
- `crates/transfer/src/token_reader.rs` - `TokenReader::read_token` /
  `see_token` / `reset`.
- `crates/engine/src/concurrent_delta/parallel_apply.rs` - target applier
  the parallel arm will dispatch into.
- `docs/design/pip-9-parallel-receive-wireup.md` section 1.1 - call-site
  cutover identification this audit supports.
