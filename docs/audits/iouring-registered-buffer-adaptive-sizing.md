# io_uring registered-buffer pool: adaptive sizing follow-up

Tracking issue: oc-rsync #2045. Companion to
[`io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md)
(telemetry-first phase) and
[`iouring-fixed-buffer-registration.md`](iouring-fixed-buffer-registration.md)
(registration mechanics). This note focuses narrowly on the static
sizing of the registered-buffer pool, the failure mode it exhibits
under sustained submission pressure, the grow / shrink heuristic that
would close the gap, and how the design contrasts with the existing
engine-level `BufferPool` adaptive resizer landed under #1638.

## 1. Current static sizing

`RegisteredBufferGroup` is allocated once per owner at ring
construction and never resized. The dimensions come from
`IoUringConfig`:

- `crates/fast_io/src/io_uring/config.rs:353` -
  `registered_buffer_count: usize`.
- `crates/fast_io/src/io_uring/config.rs:373-379` - `Default`:
  `buffer_size = 64 KiB`, `registered_buffer_count = 8`.
- `crates/fast_io/src/io_uring/config.rs:388-399` -
  `for_large_files`: `buffer_size = 256 KiB`,
  `registered_buffer_count = 16`.
- `crates/fast_io/src/io_uring/config.rs:404-413` -
  `for_small_files`: `buffer_size = 16 KiB`,
  `registered_buffer_count = 8`.

Footprint per owner (pinned via `IORING_REGISTER_BUFFERS`,
charged against `RLIMIT_MEMLOCK`):
`Default = 512 KiB`, `for_large_files = 4 MiB`,
`for_small_files = 128 KiB`. The kernel cap is 1024 buffers
(`MAX_REGISTERED_BUFFERS` in `registered_buffers.rs:80`). No code
path adjusts `count` after construction; the only escape valve is
silent fallback to non-registered `IORING_OP_READ` / `WRITE` when
`RegisteredBufferGroup::checkout` returns `None`.

## 2. Failure mode under sustained submission pressure

`flush_buffer` and `write_all_batched`
(`crates/fast_io/src/io_uring/file_writer.rs:215-308`) and
`read_all_batched` (`file_reader.rs:158-184`) each speculatively
check out `min(reg.available(), sq_entries)` slots before submission.
At high queue depth - large-file copies, SQPOLL drain bursts, dirty
writeback under memory pressure - every slot is in flight against
unfinished CQEs. The next `checkout()` returns `None`, the writer
quietly falls back to the unfixed opcode, and the per-SQE
`get_user_pages()` cost reappears. The miss is now visible in
`RegisteredBufferStats { total_acquires, total_misses }`
(`registered_buffers.rs:144-163`, landed for #2045 phase 1) but no
control loop consumes the signal. Sustained miss-rate above ~20%
during a multi-GB transfer means the pool is starved; we pay
registration cost for zero benefit on the SQEs that miss.

## 3. Proposed adaptive grow / shrink

A `RegisteredBufferSizer` runs off the I/O hot path (between
batches, never inside `submit_and_wait`), reads
`RegisteredBufferGroup::stats()`, and decides:

- **Grow** when EMA-smoothed miss rate >= `GROW_THRESHOLD` (0.20)
  and `count < min(MAX_COUNT, kernel_cap)`. New count =
  `min(count * 2, MAX_COUNT, kernel_cap)`.
- **Shrink** when EMA-smoothed miss rate <= `SHRINK_THRESHOLD`
  (0.02) AND average occupancy `(count - available) / count <=
  0.30` over the window AND `count > MIN_COUNT`. New count =
  `max(count / 2, MIN_COUNT)`.
- **Hold** otherwise. Hysteresis = 4x ratio between grow and shrink
  thresholds prevents oscillation.

Resize is a `unregister_buffers()` then fresh
`RegisteredBufferGroup::new()` cycle. The ring itself is reused;
only the pinned buffer set churns. Triggered every
`CHECK_INTERVAL = 64` checkouts, gated by warmup
(`WARMUP_SAMPLES = 8`), with a per-owner cooldown of one resize
per 250 ms to bound `RLIMIT_MEMLOCK` churn. Bounds:
`MIN_COUNT = 4`, `MAX_COUNT = 64` (well below the 1024 kernel
ceiling), `buffer_size` held constant - only `count` adapts in
phase 2.

## 4. Comparison to engine BufferPool adaptive (#1638)

| Aspect | Engine `BufferPool` (#1638) | Proposed registered-buffer sizer |
|---|---|---|
| Source | `crates/engine/src/local_copy/buffer_pool/pressure.rs` | New `crates/fast_io/src/io_uring/registered_buffer_sizer.rs` |
| Signal | `(hits, misses, ops)` per `CHECK_INTERVAL = 64` | EMA of `total_misses / total_acquires`, same interval |
| Grow trigger | miss rate > 0.20 | miss rate >= 0.20 (parity) |
| Shrink trigger | utilization < 0.30 | utilization <= 0.30 AND miss rate <= 0.02 |
| Bounds | `MIN_CAPACITY = 2`, `MAX_CAPACITY = 256` | `MIN_COUNT = 4`, `MAX_COUNT = 64` (kernel cap 1024) |
| Resize cost | `Vec` push / pop, no syscall | `unregister_buffers` + `register_buffers` syscalls, kernel re-pins pages |
| Memory footprint | Heap, swappable | `RLIMIT_MEMLOCK`-charged, never swapped |
| Frequency cap | None (cheap) | One resize per 250 ms per owner |
| Failure on resize | Cannot fail (pure userspace) | Can fail on `EAGAIN` / `ENOMEM` / `RLIMIT`; keep old group on failure |

The engine pool resizes a heap `Vec`; pressure on it costs an
allocation. The registered-buffer pool resizes pinned kernel memory;
pressure on it costs a syscall and a re-pin. The frequency cap and
the Boolean `AND` on the shrink trigger are the only behavioural
differences - both compensate for the syscall cost. Telemetry
counter shape (`total_acquires`, `total_misses`) deliberately mirrors
the `(total_hits, total_misses, total_growths)` triple in
`local_copy/buffer_pool/pool.rs:868-878` so a future shared
`PoolPressureSnapshot` trait can unify both surfaces.
