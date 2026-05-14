# io_uring fixed-buffer registration: invariants audit

Tracking issue: oc-rsync #2118.

This audit complements three sibling write-ups already in the tree by
focusing exclusively on the five operational invariants the
`RegisteredBufferGroup` wrapper must uphold around `IORING_REGISTER_BUFFERS`,
`IORING_OP_READ_FIXED`, `IORING_OP_WRITE_FIXED`, and
`IORING_UNREGISTER_BUFFERS`. Each invariant is mapped to a single
`file:line` citation, a status verdict, and a short note. The siblings
cover broader context:

- [`io-uring-fixed-buffer-audit.md`](io-uring-fixed-buffer-audit.md) -
  PR #3754 surface map: every call site and lifecycle phase.
- [`io-uring-fixed-buffer-registration.md`](io-uring-fixed-buffer-registration.md) -
  coverage view: which SQEs reach `READ_FIXED` / `WRITE_FIXED` today.
- [`iouring-fixed-buffer-registration.md`](iouring-fixed-buffer-registration.md) -
  registration-moment drill-down: pinning, `RLIMIT_MEMLOCK`, failure taxonomy.

Related work referenced below: `BgidAllocator` (PR #4005 / #4019, task
#2044) at [`io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md),
provided-buffer-ring footprint (PR #4014, task #1936) at
[`iouring-pbuf-ring.md`](iouring-pbuf-ring.md), engine local-copy buffer
pool (task #2045) at [`buffer-pool-capacity-sizing.md`](buffer-pool-capacity-sizing.md).

## Summary verdict

All five core invariants - registration at ring setup, silent fallback to
unfixed opcodes, page pinning for the ring's lifetime, sizing within
kernel-recommended ranges, and kernel-side unregister at ring fd close -
are upheld with passing tests. Three minor wrinkles persist, none of
which break correctness: `IoUringWriter::with_ring` ignores the
`IoUringConfig::register_buffers` flag, the userspace `Drop` deliberately
does not invoke `IORING_UNREGISTER_BUFFERS` (kernel cleanup on ring fd
close is relied upon), and the `MAX_REGISTERED_BUFFERS = 1024` ceiling is
a static cap rather than a kernel-version probe. The invariants table
below enumerates each finding with file:line evidence; the only proposed
fix is a one-line plumbing change for the `with_ring` flag wart, which
is left for a separate follow-up PR.

## Invariants table

| Invariant | Code location | Status | Note |
|---|---|---|---|
| Registration is performed once at ring setup, not lazily or per-transfer | `crates/fast_io/src/io_uring/registered_buffers.rs:307` (`submitter().register_buffers(&iovecs)`); owners call `try_new` from `file_reader.rs:73-81`, `file_writer.rs:56-64,83-91,118,143-151`, `shared_ring.rs:265-273` | OK | Eager, at-ring-init model. No `register_buffers` call site exists outside `RegisteredBufferGroup::new`; rebuild / resize is not implemented. |
| Construction requires Linux >= 5.6 via `is_io_uring_available()` | `crates/fast_io/src/io_uring/config.rs:19` (`MIN_KERNEL_VERSION = (5, 6)`); gated by `SharedRing::try_new` at `shared_ring.rs:221-223` | OK | `IORING_REGISTER_BUFFERS` has shipped since 5.1; the 5.6 floor covers every opcode this crate uses. `IoUringReader`/`IoUringWriter` build the ring via `config.build_ring()` which returns `io::Error` on older kernels, and `try_new` swallows the error. |
| Registration failure is surfaced as a recoverable `io::Error` and downgraded to `None` by the best-effort entry point | `crates/fast_io/src/io_uring/registered_buffers.rs:308-315` (error path frees buffers and returns `io::Error::other`); `registered_buffers.rs:351-353` (`try_new` swallows to `Option`) | OK | `try_new` is the only path used by production owners; `new` is reserved for tests and explicit callers that want the error. |
| Silent fallback: when `registered_buffers == None`, reads / writes use the unfixed opcodes | `crates/fast_io/src/io_uring/file_reader.rs:158-184` (READ_FIXED guard) followed by `file_reader.rs:186-275` (unfixed `opcode::Read`); `file_writer.rs:215-246` and `file_writer.rs:282-309` (WRITE_FIXED guards) followed by `batching.rs::submit_write_batch` | OK | The fallback also covers the runtime "all slots in use" case via the same `available() > 0` precheck (`file_reader.rs:159`, `file_writer.rs:216`, `file_writer.rs:283`); misses are counted in `RegisteredBufferStats` (`registered_buffers.rs:117-121,386,410`). |
| Page-aligned allocation: every buffer is rounded up to `_SC_PAGESIZE` | `crates/fast_io/src/io_uring/registered_buffers.rs:271-278` (`buffer_size.next_multiple_of(page_size)`, `Layout::from_size_align`); `registered_buffers.rs:478-485` (`page_size()` via `sysconf(_SC_PAGESIZE)`) | OK | Required for DMA-friendly buffers; `Layout::from_size_align` rejects non-power-of-two alignment so the fallback `4096` in `page_size()` is also page-of-two-clean. |
| Buffer pages stay pinned for the ring's lifetime | `crates/fast_io/src/io_uring/registered_buffers.rs:124-130` (raw pointers, `Send`+`Sync` impls); user-side memory owned exclusively by the group; `RegisteredBufferSlot` borrows `&self` so no slot outlives the group (`registered_buffers.rs:169-173,232-236`) | OK | Pinning is by ownership: the group holds raw pointers that are never reallocated or moved. Kernel-side pinning is held by the registered iovecs until the ring closes. |
| Drop ordering: `RawIoUring` field declared before `RegisteredBufferGroup` so ring fd closes first, releasing kernel pinning before userspace dealloc | `crates/fast_io/src/io_uring/file_reader.rs:31,40` (`ring` before `registered_buffers`); `file_writer.rs:31,41` (same); `shared_ring.rs:193,204` (same); documented at `registered_buffers.rs:18-39` | OK | Tests `drop_ring_before_group_frees_memory_cleanly` (`registered_buffers.rs:1075-1091`), `drop_group_before_ring_does_not_panic` (`registered_buffers.rs:1042-1068`), and `struct_field_drop_order_matches_callers` (`registered_buffers.rs:1097-1119`) cover both orderings. |
| Default sizing: 8 buffers x 64 KiB, page-aligned, with named presets for large / small files | `crates/fast_io/src/io_uring/config.rs:367-381` (`default()`: `count = 8`, `buffer_size = 64 * 1024`); `config.rs:386-398` (`for_large_files`: `16 x 256 KiB`); `config.rs:401-414` (`for_small_files`: `8 x 16 KiB`) | OK | The kernel does not publish a recommended range; the documented limit is `IORING_MAX_REG_BUFFERS = 1024` (`io_uring/rsrc.c`). The 8 / 64 KiB default is empirical, sized to saturate commodity NVMe with eight in-flight reads. The 256 KiB large-file buffer matches `BIO_MAX_VECS * PAGE_SIZE` on modern kernels and is the largest contiguous transfer the block layer handles without splitting. |
| Hard upper bound: `MAX_REGISTERED_BUFFERS = 1024` matches the kernel's `IORING_MAX_REG_BUFFERS` constant | `crates/fast_io/src/io_uring/registered_buffers.rs:80` (`const MAX_REGISTERED_BUFFERS: usize = 1024`); enforced at `registered_buffers.rs:264-269` | OK | Kernel header constant: `include/uapi/linux/io_uring.h::IORING_MAX_REG_BUFFERS`. Static cap; no probe for kernel-version differences. |
| `buf_index` slot identifier fits the kernel's `u16` SQE field | `crates/fast_io/src/io_uring/registered_buffers.rs:172` (`index: u16`); `registered_buffers.rs:179`, `registered_buffers.rs:403` (`(word_idx * 64 + bit) as u16`) | OK | `MAX_REGISTERED_BUFFERS = 1024` fits in `u16` with three bits to spare; the SQE definition uses `__u16 buf_index` in `include/uapi/linux/io_uring.h::io_uring_sqe`. |
| Slot checkout / return is lock-free and panic-safe | `crates/fast_io/src/io_uring/registered_buffers.rs:386-412` (CAS loop on atomic bitset); `registered_buffers.rs:232-236` (`Drop` returns slot); `registered_buffers.rs:434-439` (`return_slot` releases bit) | OK | Test `panic_during_slot_use_unwinds_cleanly` (`registered_buffers.rs:1125-1152`) confirms unwinding returns the slot and leaves the group reusable. |
| Drop deallocates all userspace memory, never calls `IORING_UNREGISTER_BUFFERS` | `crates/fast_io/src/io_uring/registered_buffers.rs:451-473` (`Drop::drop`); rationale documented at `registered_buffers.rs:41-49` | OK | Intentional. The kernel's `io_sqe_buffers_unregister` (linked from `io_uring/rsrc.c`) runs as part of the ring fd's `release` handler when the file is closed. Holding a ring reference inside the group would force lifetime coupling that breaks the documented drop order. `panic` safety preserved because `alloc::dealloc` does not panic. |
| Explicit unregister API exists for callers that want deterministic cleanup while keeping the ring alive | `crates/fast_io/src/io_uring/registered_buffers.rs:446-448` (`unregister(&ring)` -> `submitter().unregister_buffers()`) | OK | Used by tests at `registered_buffers.rs:1158-1180,1187-1221`; not used by production owners. |
| Process termination releases kernel pinning even without `Drop` | Documented at `crates/fast_io/src/io_uring/registered_buffers.rs:58-62` | OK | On SIGKILL / abort, the kernel's task-exit path closes all fds, which invokes the same `io_sqe_buffers_unregister` cleanup. No leak persists across process boundaries. |
| `read_at` / `write_at` one-shot paths intentionally use unfixed `IORING_OP_READ` / `IORING_OP_WRITE` | `crates/fast_io/src/io_uring/file_reader.rs:111-114`; `crates/fast_io/src/io_uring/file_writer.rs:177-181` | OK | One-shot reads write into the caller's `&mut [u8]` directly; using `READ_FIXED` would require a `memcpy` out of the slot, defeating the saving on short transfers. |
| `with_ring` ignores `IoUringConfig::register_buffers` and hard-codes `count = 8` | `crates/fast_io/src/io_uring/file_writer.rs:118` (`RegisteredBufferGroup::try_new(&ring, buffer_capacity, 8)`) | GAP | A caller that explicitly disables registration in `IoUringConfig` still pays the allocation when the writer is constructed via `super::writer_from_file`. Proposed fix: change the `with_ring` signature to take a `&IoUringConfig` (or at minimum an explicit `register: bool` / `count: usize` pair) and forward both values from the `writer_from_file_with_depth` caller in `mod.rs:166-218`. Scope: one signature change, two call-site updates, no public-API impact outside the `super` boundary. |
| `RegisteredBufferStats` distinguishes runtime checkout misses but not "disabled" vs "registration failed" | `crates/fast_io/src/io_uring/registered_buffers.rs:144-163,386,410-411,425-431`; consumed by adaptive sizer per `io-uring-adaptive-buffer-sizing.md` | UNKNOWN | Documented as a known telemetry blind spot in the sibling audit `iouring-fixed-buffer-registration.md` Section 5a. Not a correctness gap; an observability gap. Status flagged UNKNOWN because the impact depends on whether a `RegisteredBufferStatus` enum is desired in the public API. Out of scope for this docs-only audit. |
| `MAX_REGISTERED_BUFFERS` is a static cap, not a probe of the running kernel | `crates/fast_io/src/io_uring/registered_buffers.rs:80` | OK | Kernel constants have not moved since the cap was introduced; matches `IORING_MAX_REG_BUFFERS` on every kernel >= 5.1. If a future kernel lowers the limit, the kernel returns `EINVAL` and `try_new` falls back, so the cap is an early-rejection optimisation rather than a correctness lever. |
| `RegisteredBufferSlot` ensures slot indices are in-bounds and exclusively owned | `crates/fast_io/src/io_uring/registered_buffers.rs:188-229` (slice / pointer accessors clamp to `buffer_size`); `registered_buffers.rs:165-172` (slot borrows `&group`) | OK | `debug_assert!(len <= self.group.buffer_size)` in `as_slice` / `as_mut_slice` plus the `.min(buffer_size)` clamp guarantees out-of-bounds slicing is unreachable in release. |

## Proposed fix for the single GAP row

`IoUringWriter::with_ring` at `crates/fast_io/src/io_uring/file_writer.rs:118`
currently calls
`RegisteredBufferGroup::try_new(&ring, buffer_capacity, 8)` unconditionally,
ignoring `IoUringConfig::register_buffers` and the configured
`registered_buffer_count`. The natural fix:

1. Extend the `with_ring` signature in `file_writer.rs:110-116` to accept
   the relevant config fields (or the whole `&IoUringConfig`).
2. Forward `config.register_buffers` and `config.registered_buffer_count`
   from `writer_from_file_with_depth` at
   `crates/fast_io/src/io_uring/mod.rs:166-218`.
3. Mirror the `if config.register_buffers { try_new(...) } else { None }`
   guard already used by `create` / `from_file` / `create_with_size`.

Out of scope for this audit (docs-only). Recorded in this table so a
follow-up PR can close the gap without rediscovery.

## Cross-references

- `BgidAllocator` (PR #4005 / #4019, task #2044) - manages the disjoint
  `IORING_REGISTER_PBUF_RING` (provided-buffer-group) namespace, kernel
  5.19+. Not the same kernel object as the iovec slot table audited
  here, but governs a separate id space; see
  [`io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md) and
  [`io-uring-bgid-exhaustion.md`](io-uring-bgid-exhaustion.md).
- Buffer-ring footprint (PR #4014, task #1936) - sizing model and
  per-bgid memory accounting for the `IORING_OP_PROVIDE_BUFFERS` path;
  see [`iouring-pbuf-ring.md`](iouring-pbuf-ring.md). Mentioned because
  oc-rsync uses both namespaces independently: this audit covers the
  iovec-slot path only.
- Engine local-copy buffer pool (task #2045) - userspace `BufferPool` in
  `crates/engine/src/local_copy/buffer_pool/pool.rs` shares the
  acquire / miss / hit-rate telemetry pattern with
  `RegisteredBufferStats`; the adaptive-sizing design at
  [`buffer-pool-capacity-sizing.md`](buffer-pool-capacity-sizing.md)
  and [`io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md)
  is the long-term home for tuning both.

## Verification surface

The invariants above are exercised by tests in the same file:

- Sizing / construction: `registered_buffer_group_create_and_checkout`
  (`registered_buffers.rs:756`), rejection tests at lines 726-753.
- Drop ordering: `drop_group_before_ring_does_not_panic`
  (`registered_buffers.rs:1042`),
  `drop_ring_before_group_frees_memory_cleanly`
  (`registered_buffers.rs:1075`),
  `struct_field_drop_order_matches_callers`
  (`registered_buffers.rs:1097`).
- Panic safety: `panic_during_slot_use_unwinds_cleanly`
  (`registered_buffers.rs:1125`).
- Unregister semantics: `unregister_after_ring_closed_returns_error_or_ok`
  (`registered_buffers.rs:1158`),
  `buffers_freed_with_or_without_explicit_unregister`
  (`registered_buffers.rs:1188`).
- Telemetry: `stats_initially_zero`, `stats_count_successful_checkouts`,
  `stats_count_misses_on_exhaustion`, `stats_not_decremented_on_return`,
  `stats_miss_rate_zero_when_no_acquires`, `stats_miss_rate_all_misses`
  (`registered_buffers.rs:1224-1338`).

Upstream rsync 3.4.1 has no io_uring path; this audit covers an oc-rsync
local optimisation with no wire-protocol implication. Kernel-side
behaviour cited above is verifiable against `io_uring/rsrc.c::io_sqe_buffers_register`
and `io_sqe_buffers_unregister` in any kernel >= 5.6 source tree.
