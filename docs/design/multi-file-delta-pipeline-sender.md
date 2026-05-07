# Sender-Side Multi-File Delta Pipeline (#1270)

## 1. Scope and Distinction from Related Work

This design covers the **sender-side** delta-generation pipeline: the host that
produces `MATCH`/`DATA` token streams from the basis signature it received from
the receiver. It is the dual of #1079, which redesigns the **receiver-side**
apply pipeline (token-stream consumption and basis-block reads). It also reuses
the parallel rolling-hash fan-out delivered by #2048 (per-file hash-bucket
construction), promoting that primitive from per-file to inter-file scope.

- #1079: receiver-side parallel apply (token consumption, basis-block fetch).
- #2048: per-file parallel rolling-hash construction (done).
- #1270 (this doc): cross-file delta generation in flight concurrently while
  preserving the single serial token stream upstream rsync expects.

## 2. Current Behaviour

The sender's `send_files()` loop processes one file at a time:

1. Read receiver-supplied `sum_struct` (block hashes) for file `N`.
2. Open basis source, scan with rolling checksum, emit `MATCH`/`DATA` tokens.
3. Flush, advance to file `N+1`.

CPU-bound work (rolling hash window slide, strong-checksum verification on
candidate matches, optional zlib/zstd compression on `DATA` runs) executes on a
single core while the network and disk-read I/O sit idle. Workloads with many
small/medium files leave 60-80% of available cores unused.

## 3. Constraints

- Wire output must remain byte-identical: token frames per file in the same
  order upstream rsync emits them, and files in the order received from the
  generator's file list.
- Sequence-numbered work units already exist (#1546, done): every file carries
  an `flist_idx` that doubles as its emission rank.
- Multiplex framing (`MSG_DATA`) is the sole writer; no per-worker socket access
  is permitted.

## 4. Design

```
file list ─► DeltaWork{idx, basis, sum_struct}
              │
              ▼
       rayon par_iter (bounded pool, N = num_cpus)
              │   compute MATCH/DATA tokens + optional compress
              ▼
        ReorderBuffer (BTreeMap<idx, TokenBuf>)
              │   pop while head == next_emit_idx
              ▼
        single emitter thread ─► multiplex writer ─► socket
```

- `DeltaWork` is a self-contained unit: receiver's block-hash table, basis-file
  handle (or memory map), output buffer, sequence index. No shared mutable
  state between workers.
- The ReorderBuffer caps in-flight files to bound memory; back-pressure stalls
  the producer when the head-of-line file has not finished.
- Compression, if enabled, runs inside the worker so the emitter only copies
  pre-framed bytes to the multiplex writer.
- Reuses #2048's parallel rolling-hash for large files: a single worker may
  internally fan out hash construction without changing the outer ordering.

## 5. Risks

- **SIMD parity (#2077 pending).** Worker threads exercise SIMD rolling-hash
  paths concurrently; parity tests must run under thread-stress before this
  ships. Block on #2077.
- **Wire format unchanged (#1548 pending verification).** Golden byte tests
  must confirm token-stream equivalence vs the serial path on every supported
  protocol version (28-32). Block on #1548.
- **Memory ceiling.** A naive ReorderBuffer can buffer arbitrary files when one
  large file stalls many small ones; bound by `max_in_flight_bytes` plus a
  per-file cap, fall back to serial when exceeded.
- **Determinism under errors.** A worker failure must surface at the emit
  point in flist order so existing exit-code semantics are preserved.
