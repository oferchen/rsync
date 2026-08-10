# IUS-8.a - Author the `IoUringBackend` trait surface

Date: 2026-05-24
Scope: docs-only deliverable that fixes the exact trait surface
IUS-8.b (Linux impl) and IUS-8.c (non-Linux stub collapse) implement
against. Derives from IUS-7.a (trait shape) and IUS-7.b (zero-cost
gate). Adds: module placement, file-by-file edit list, method-to-
wrapper cross-reference, path A/B decision, stub return-value table.

Status: **SPEC DRAFT** - no source changes in this PR.
Downstream: IUS-8.b (Linux `LinuxIoUringBackend` impl), IUS-8.c
(non-Linux `StubIoUringBackend` impl, deletes the 2,479-LoC mirror
tree at `crates/fast_io/src/io_uring_stub/`).

Inventory verified at this doc's date:

- `crates/fast_io/src/io_uring/`: 22 files, 13,665 LoC (with tests).
- `crates/fast_io/src/io_uring_stub/`: 21 files, 2,479 LoC (2,070
  non-test, 409 test).
- `crates/fast_io/src/io_uring_common.rs`: cross-platform plain-data
  types. Stays as-is; both impls re-export from here.

## 1. Module placement

```
crates/fast_io/src/
+- io_uring/
|  +- backend.rs              NEW (IUS-8.b): trait + types, platform-free
|  +- backend_impl.rs         NEW (IUS-8.b): Linux impl, cfg-gated
|  +- backend_stub.rs         NEW (IUS-8.c): stub impl, cfg-gated
|  +- mod.rs                  EDIT: cfg-gated re-exports
|  +- ...existing wrappers stay untouched...
+- io_uring_stub/             DELETED in IUS-8.c
+- io_uring_common.rs         unchanged
+- lib.rs                     EDIT: cfg-gated public re-export
```

`backend.rs` compiles on every supported target (Linux, macOS,
Windows). `backend_impl.rs` is gated on `#[cfg(all(target_os =
"linux", feature = "io_uring"))]`; `backend_stub.rs` on the inverse.
No new Cargo dependencies.

## 2. Trait surface (authoring deliverable)

The trait body is fixed in IUS-7.a section 1. This section is its
build-list: every method, the existing function it forwards to on
Linux, and the source file IUS-8.b lifts the forwarder body from.

### 2.1 `IoUringBackend` (38 methods)

| # | Method | Forwards to | Source file | Path (IUS-7.b sec 5.1) |
|---|---|---|---|---|
| 1 | `is_available()` | `config::is_io_uring_available()` | `io_uring/config.rs` | cold |
| 2 | `availability_reason()` | `config::config_detail::io_uring_availability_reason()` | `io_uring/config.rs` | cold |
| 3 | `sqpoll_fell_back()` | `config::sqpoll_fell_back()` | `io_uring/config.rs` | cold |
| 4 | `kernel_info()` | `config::config_detail::io_uring_kernel_info()` | `io_uring/config.rs` | cold |
| 5 | `build_ring(cfg)` | `shared_ring::SharedRing::with_config(cfg)` | `io_uring/shared_ring.rs` | cold |
| 6 | `submit_one(ring, sqe)` | match-arm dispatch (see 2.6) | many | **HOT** |
| 7 | `submit_batch(ring, sqes)` | loop + `submit_and_wait(0)` | `io_uring/shared_ring.rs` | **HOT** |
| 8 | `submit_and_wait(ring, n)` | `SharedRing::submit_and_wait(n)` | `io_uring/shared_ring.rs` | **HOT** |
| 9 | `drain_completions(ring)` | `SharedRing::reap()` iterator | `io_uring/shared_ring.rs` | **HOT** |
| 10 | `register_buffers(ring, bufs)` | `RegisteredBufferGroup::register(...)` | `io_uring/registered_buffers/registry.rs` | cold |
| 11 | `unregister_buffers(ring, id)` | `RegisteredBufferGroup::drop` | `io_uring/registered_buffers/registry.rs` | cold |
| 12 | `register_files(ring, fds)` | `SharedRing::register_files(fds)` | `io_uring/shared_ring.rs` | cold |
| 13 | `unregister_files(ring)` | `SharedRing::unregister_files()` | `io_uring/shared_ring.rs` | cold |
| 14 | `register_provided_buffer_ring(ring, cfg)` | `BufferRing::register(cfg)` | `io_uring/buffer_ring/registration.rs` | cold |
| 15 | `registered_buffer_stats(ring)` | `RegisteredBufferGroup::stats()` | `io_uring/registered_buffers/stats.rs` | cold |
| 16 | `registered_buffer_status(ring)` | `RegisteredBufferGroup::status()` | `io_uring/registered_buffers/registry.rs` | cold |
| 17 | `probe_op(op)` | cached `u128` bitmap from `kernel_info().supported_ops` | `io_uring/config.rs` | **HOT** (cached) |
| 18 | `statx_supported()` | default -> `probe_op(Statx)` | `io_uring/statx.rs` | **HOT** (cached) |
| 19 | `linkat_supported()` | default -> `probe_op(Linkat)` | `io_uring/linkat.rs` | **HOT** (cached) |
| 20 | `renameat2_supported()` | default -> `probe_op(Renameat)` | `io_uring/renameat2.rs` | **HOT** (cached) |
| 21 | `send_zc_supported()` | `send_zc::is_supported()` | `io_uring/send_zc.rs` | **HOT** (cached) |
| 22 | `pbuf_ring_supported()` | `buffer_ring::pbuf_ring_supported()` | `io_uring/buffer_ring/mod.rs` | **HOT** (cached) |
| 23 | `cancel_supported()` | `cancel::ASYNC_CANCEL_MIN_KERNEL` probe | `io_uring/cancel.rs` | **HOT** (cached) |
| 24 | `cancel_by_fd_supported()` | `cancel::ASYNC_CANCEL_FD_MIN_KERNEL` probe | `io_uring/cancel.rs` | **HOT** (cached) |
| 25 | `allocate_bgid()` | `bgid_lease::with_thread_lease(|l| l.take())` (see 8.1) | `io_uring/bgid_lease.rs` | cold |
| 26 | `deallocate_bgid(id)` | `BgidAllocator::deallocate(id)` | `io_uring/buffer_ring/allocator.rs` | cold |
| 27 | `bgid_remaining()` | `BgidAllocator::remaining()` | `io_uring/buffer_ring/allocator.rs` | cold |
| 28 | `submit_statx_blocking(...)` | `statx::submit_statx_blocking(...)` | `io_uring/statx.rs` | cold |
| 29 | `submit_statx_batch(...)` | `statx::submit_statx_batch(...)` | `io_uring/statx.rs` | warm |
| 30 | `submit_linkat_blocking(...)` | `linkat::submit_linkat_blocking(...)` | `io_uring/linkat.rs` | cold |
| 31 | `submit_renameat2_blocking(...)` | `renameat2::renameat2_blocking(...)` | `io_uring/renameat2.rs` | cold |
| 32 | `build_session_pool(cfg)` | `SessionRingPool::new(cfg)` | `io_uring/session_pool.rs` | cold |
| 33 | `build_shared_ring(r, w, cfg)` | `SharedRing::new_pair(r, w, cfg)` | `io_uring/shared_ring.rs` | cold |
| 34 | `open_reader(path, cfg)` | `IoUringReaderFactory::open(path)` | `io_uring/file_factory.rs` | cold |
| 35 | `open_writer(path, cfg)` | `IoUringWriterFactory::create(path)` | `io_uring/file_factory.rs` | cold |
| 36 | `writer_from_file(file, cap, cfg)` | top-level `writer_from_file(...)` | `io_uring/mod.rs` | cold |
| 37 | `build_disk_batch(cfg)` | `IoUringDiskBatch::new(cfg)` | `io_uring/disk_batch.rs` | cold |

Method count: 37 distinct bodies + 7 default-impl probe shortcuts
share method 17's cache. IUS-7.a counts these as 38 on the primary
trait; this doc keeps that count.

### 2.2 Auxiliary traits (19 methods)

| Trait | Method | Forwards to | Source file | Path |
|---|---|---|---|---|
| `RingHandle` | `sq_entries()` | `SharedRing::sq_entries()` | `io_uring/shared_ring.rs` | cold |
| `RingHandle` | `sqpoll_active()` | `SharedRing::sqpoll_active()` | `io_uring/shared_ring.rs` | cold |
| `SessionPool` | `ring_count()` | `SessionRingPool::len()` | `io_uring/session_pool.rs` | cold |
| `SessionPool` | `acquire()` | `SessionRingPool::acquire()` | `io_uring/session_pool.rs` | cold |
| `SessionLease` | `slot()` | `RingLease::slot()` | `io_uring/session_pool.rs` | cold |
| `SharedRingHandle` | `reader_slot()` | `SharedRing::reader_slot()` | `io_uring/shared_ring.rs` | cold |
| `SharedRingHandle` | `writer_slot()` | `SharedRing::writer_slot()` | `io_uring/shared_ring.rs` | cold |
| `SharedRingHandle` | `poll_add_supported()` | `SharedRing::poll_add_supported()` | `io_uring/shared_ring.rs` | cold (cached) |
| `SharedRingHandle` | `has_registered_buffers()` | `SharedRing::has_registered_buffers()` | `io_uring/shared_ring.rs` | cold |
| `SharedRingHandle` | `submit_read(...)` | `SharedRing::submit_read(...)` | `io_uring/shared_ring.rs` | **HOT** |
| `SharedRingHandle` | `submit_send(...)` | `SharedRing::submit_send(...)` | `io_uring/shared_ring.rs` | **HOT** |
| `SharedRingHandle` | `submit_poll_write(...)` | `SharedRing::submit_poll_write(...)` | `io_uring/shared_ring.rs` | **HOT** |
| `SharedRingHandle` | `submit_and_wait(n)` | `SharedRing::submit_and_wait(n)` | `io_uring/shared_ring.rs` | **HOT** |
| `SharedRingHandle` | `reap()` | `SharedRing::reap()` | `io_uring/shared_ring.rs` | **HOT** |
| `DiskBatch` | `begin_file(file)` | `IoUringDiskBatch::begin_file(file)` | `io_uring/disk_batch.rs` | cold |
| `DiskBatch` | `write_data(data)` | `IoUringDiskBatch::write_data(data)` | `io_uring/disk_batch.rs` | **HOT** |
| `DiskBatch` | `commit_file(fsync)` | `IoUringDiskBatch::commit_file(fsync)` | `io_uring/disk_batch.rs` | cold |
| `DiskBatch` | `bytes_written()` | `IoUringDiskBatch::bytes_written()` | `io_uring/disk_batch.rs` | cold |
| `DiskBatch` | `bytes_written_with_pending()` | `IoUringDiskBatch::bytes_written_with_pending()` | `io_uring/disk_batch.rs` | cold |

**Total: 57 methods across 5 traits (38 primary + 19 auxiliary).**

### 2.3 `submit_one` match-arm table

The `submit_one` body dispatches `SubmissionEntry` variants to the
existing opcode wrappers. Each arm is 1-3 lines.

| `SubmissionEntry` variant | Linux dispatch |
|---|---|
| `Read { fd, buf, offset, .. }` | `SharedRing::submit_read(...)` |
| `Write { fd, buf, offset, .. }` | direct `io-uring` crate `Write` build + `SharedRing::push_sqe` |
| `ReadFixed { fd, buf_index, buf_ptr, len, offset, .. }` | `registered_buffers::submit::push_read_fixed(...)` |
| `WriteFixed { fd, buf_index, buf_ptr, len, offset, .. }` | `registered_buffers::submit::push_write_fixed(...)` |
| `Recv { fd, buf, .. }` | direct `io-uring` crate `Recv` build |
| `Send { fd, buf, .. }` | `SharedRing::submit_send(...)` |
| `SendZc { fd, buf, .. }` | `send_zc::try_send_zc(...)` (returns `OpcodeUnsupported` when `iouring-send-zc` is off) |
| `Fsync { fd, .. }` | direct `io-uring` crate `Fsync` build |
| `PollAdd { fd, events, .. }` | direct `io-uring` crate `PollAdd` build |
| `LinkTimeout { timeout, .. }` | direct `io-uring` crate `LinkTimeout` build |
| `Statx { ... }` | `statx::build_statx_sqe_unchecked(...)` + push to SQ |
| `Renameat2 { ... }` | `renameat2::build_renameat2_sqe_unchecked(...)` + push to SQ |
| `Linkat { ... }` | `linkat::build_linkat_sqe_unchecked(...)` + push to SQ |
| `CancelByUserData { ... }` | `cancel::cancel_by_user_data(...)` |
| `CancelByFd { ... }` | `cancel::cancel_all_by_fd(...)` |

15 arms, one per opcode in the IUS-7.a enum.

## 3. Path A vs path B - associated types vs generic parameters

| Path | Sketch | Pro | Con |
|---|---|---|---|
| A: associated type | `trait IoUringBackend { type Ring: RingHandle; fn submit_one(&self, r: &mut Self::Ring, ...) }` | Per-backend ring shape fixed at impl time; callers write `<B::Ring>` once. | Not object-safe; `dyn IoUringBackend` requires the `BoxedRing` adapter from IUS-7.a section 9.2. |
| B: generic parameter | `trait IoUringBackend<R: RingHandle> { fn submit_one(&self, r: &mut R, ...) }` | Object-safe via `dyn IoUringBackend<BoxedRing>`. | Callers thread `R` everywhere; multiple impls on the same backend become ambiguous; needs `<B as IoUringBackend<R>>::submit_one` syntax to disambiguate. |

**Decision: path A.** Reasons in priority order:

1. Each platform has exactly one ring type. Path B's flexibility
   buys nothing - the Linux ring is `SharedRing`, the stub ring is
   `StubRing`. Per-thread / shared / session rings are configurations
   of `SharedRing`, not separate types.
2. Path A composes with the IUS-7.b zero-cost guarantee: hot-path
   callers say `submit_loop<B: IoUringBackend>` instead of
   `submit_loop<B, R> where B: IoUringBackend<R>` - fewer generic
   params, identical codegen.
3. Object-safety is not free under path B either. The trait's
   submission entries hold lifetimes (`&[u8]`, `&mut [u8]`, `&CStr`)
   so an `&dyn IoUringBackend<BoxedRing>` still needs the
   `SubmissionEntry<'_>` lifetime to outlive every borrow - the same
   adapter the path-A `BoxedRing` solution requires.
4. The existing `IoBackend` trait in `io_uring_common.rs` already
   uses the associated-type pattern for the information-only backend
   view. Path A keeps the operations-trait consistent.

The `DynIoUringBackend` adapter from IUS-7.a section 9.2 is the
escape hatch for the few call sites that need trait-object storage
(per-thread storage in IUR-2, future plugin entry points). IUS-8.a
authors both the primary trait (path A) and the adapter together.

## 4. Zero-cost guarantee - delivery checklist for IUS-8.b

Restating IUS-7.b in the form IUS-8.b's reviewer reads top-to-bottom:

- [ ] Every `LinuxIoUringBackend` impl method carries
      `#[inline(always)]`. Method bodies are 1-3 lines. (IUS-7.b 2.1)
- [ ] No method body allocates beyond what the forwarded wrapper
      already does (`Box::new`, `Vec::with_capacity`, `format!`,
      `String::from` forbidden). Forwarders only. (IUS-7.b 2.5)
- [ ] No method body contains `log::*` or `tracing::*`. Observation
      stays in the wrapper. (IUS-7.b 2.5)
- [ ] The 8 probe shortcuts share one `OnceLock<u128>` cache line.
      (IUS-7.b 5.1)
- [ ] `drain_completions` returns a GAT-typed iterator
      (`type Drain<'a>: Iterator<Item = CompletionEntry> + 'a where
      Self: 'a;`) to avoid per-drain `Box` allocation. (IUS-7.b 6.2,
      8.4)
- [ ] No hot-path caller takes `&dyn IoUringBackend`. Callers under
      `crates/transfer/`, `crates/engine/`, and
      `crates/fast_io/src/io_uring/` use generic dispatch only.
      (IUS-7.b 4)
- [ ] `crates/fast_io/tests/backend_asm.rs` produces an empty
      normalised diff between `submit_one_through_trait` and
      `submit_one_direct`. (IUS-7.b 3.2)
- [ ] `crates/fast_io/benches/backend_dispatch.rs` reports through-
      trait / direct ratio at or below 1.02 (2 %) for each of the 12
      hot-path methods. (IUS-7.b 3.4)

IUS-8.b's PR description includes the bench output and asm-diff
output verbatim. Anything not green blocks merge.

## 5. Stub impl return-value table

| Method category | Stub return value |
|---|---|
| Availability (`is_available`, `sqpoll_fell_back`, `*_supported`) | `false` |
| Diagnostic (`availability_reason`) | `"io_uring: disabled (not built for this target)".to_string()` |
| Kernel info | `IoUringKernelInfo { available: false, kernel_major: None, kernel_minor: None, supported_ops: 0, pbuf_ring_supported: false, reason: <as above> }` |
| Ring construction (`build_ring`, `build_shared_ring`, `build_session_pool`, `build_disk_batch`, `open_reader`, `open_writer`, `writer_from_file`) | `Err(IoUringError::Unsupported)` |
| Submission (`submit_one`, `submit_batch`, `submit_and_wait`, `submit_*_blocking`, `submit_statx_batch`) | `Err(IoUringError::Unsupported)` |
| Completion (`drain_completions`) | empty iterator (`std::iter::Empty<CompletionEntry>` via GAT) |
| Buffer / file registration | `Err(IoUringError::Unsupported)` |
| Stats / status | zeroed `RegisteredBufferStats`; `RegisteredBufferStatus::NotRegistered` |
| Probes (`probe_op`, all `*_supported`) | `false` |
| `allocate_bgid` | `Err(BgidAllocError::Exhausted { fresh_used: 0, free_list_len: 0 })` |
| `deallocate_bgid` | no-op |
| `bgid_remaining` | `0` |
| `RingHandle::sq_entries` / `sqpoll_active` | `0` / `false` |

The stub block compiles to one return per method; the optimiser
folds each body to a single `mov` + `ret` on release builds. IUS-7.a
section 6 estimates ~200 LoC total - one file vs the current 21.

## 6. IUS-8.b edit list (Linux impl)

| File | Change | Approx LoC |
|---|---|---|
| `crates/fast_io/src/io_uring/backend.rs` | NEW. Trait definitions per IUS-7.a sec 1. Types only; no impls. | ~400 |
| `crates/fast_io/src/io_uring/backend_impl.rs` | NEW. `LinuxIoUringBackend` impl with 57 forwarders. `#[inline(always)]` each. | ~600 |
| `crates/fast_io/src/io_uring/mod.rs` | EDIT. Add `pub mod backend;` and `pub mod backend_impl;`; remove the existing marker `LinuxIoUringBackend` struct (replaced). | +20 / -15 |
| `crates/fast_io/src/lib.rs` | EDIT. Add cfg-gated `pub use io_uring::backend::*;`. | +10 |
| `crates/fast_io/tests/backend_smoke.rs` | NEW. Smoke test: every trait method reaches its forwarder without panicking. | ~150 |
| `crates/fast_io/tests/backend_asm.rs` | NEW. Asm-diff fixtures per IUS-7.b sec 3.2. | ~80 |
| `crates/fast_io/benches/backend_dispatch.rs` | NEW. Criterion harness per IUS-7.b sec 3.3. | ~250 |
| `tools/ci/check_zero_cost.py` | NEW. CI gate per IUS-7.b sec 3.4. | ~80 |
| `.github/workflows/benchmarks.yml` | EDIT. Add `io-uring-zero-cost` job. | +30 |
| `tools/audit_no_dyn_in_hot_path.sh` | NEW. Grep linter per IUS-7.b sec 4. | ~40 |

**No existing `io_uring/*` wrapper is edited.** Forwarders call the
wrappers as-is; any new public entry point a forwarder needs is
purely additive.

**No caller is migrated.** Caller migration is a separate initiative
(provisional IUS-9). The trait lands first; forwarder bodies coexist
with the existing free functions for at least one release cycle.

## 7. IUS-8.c edit list (non-Linux stub)

| File / dir | Change | Approx LoC |
|---|---|---|
| `crates/fast_io/src/io_uring/backend_stub.rs` | NEW. `StubIoUringBackend` impl per section 5. | ~200 |
| `crates/fast_io/src/io_uring/mod.rs` | EDIT. Add `#[cfg(not(all(target_os = "linux", feature = "io_uring")))]` gate for `backend_stub`. | +5 |
| `crates/fast_io/src/lib.rs` | EDIT. Swap `pub use io_uring_stub::*;` for the trait-based stub re-export. | +5 / -3 |
| `crates/fast_io/src/io_uring_stub/` | DELETE. 21 files, ~2,070 non-test LoC, 409 test LoC. | -2,479 |
| `crates/fast_io/tests/backend_smoke.rs` | EDIT. Add `#[cfg(not(...))]` arm asserting every method returns `IoUringError::Unsupported`. | +50 |
| callers importing `crate::io_uring_stub::*` | EDIT. One-line replacement to `crate::io_uring::backend::*`. | varies |

**Caller audit step.** Before deleting the stub tree, the IUS-8.c
implementer runs:

```sh
git grep -nE 'crate::io_uring_stub' crates/ tools/ xtask/
```

Every hit becomes a one-line replacement. The result must compile
on Linux (`cargo check --features io_uring`), Linux-without-feature
(`cargo check --no-default-features`), Windows
(`cargo check --target x86_64-pc-windows-msvc`), and macOS
(`cargo check --target aarch64-apple-darwin`). The cross-platform
matrix is the CI gate for IUS-8.c merge.

**Net effect.** Non-Linux mirror drops from 2,479 LoC across 21
files to ~200 LoC in one file - a 12x reduction. Per-platform
signature divergence is impossible because there is only one trait
definition.

## 8. Open questions for IUS-8.b / IUS-8.c

None of these block authoring the trait surface. Each is logged so
the implementing PR has a sticky-note attached.

### 8.1 BGID lease integration (IUR-3.e)

`allocate_bgid` forwards to `bgid_lease::with_thread_lease`. A
fresh thread paying for a full `DEFAULT_LEASE_BATCH` on first call
is the design choice IUR-3.e already made; IUS-8.b inherits it. If
adversarial single-shot-allocate workloads emerge, a fast-path
check against the central pool free-list is the documented
contingency.

### 8.2 One-shot kernel probes (IUR-3.f)

IUR-3.f's shared-ring construction is what `build_shared_ring`
forwards to. The cached-probe interaction question: should
`LinuxIoUringBackend::new` populate the `OnceLock<u128>` eagerly so
every later call sees a warm cache? Recommendation: yes.
Construction cost is ~7 syscalls, amortised across the backend's
lifetime.

### 8.3 `IORING_OP_NOP` for the bench harness

IUS-7.b sec 3.3 references `SubmissionEntry::Nop` for the
through-trait micro-bench. IUS-7.a's enum does not include `Nop`.
IUS-8.b adds the variant gated behind `#[cfg(any(test, bench))]` so
the production surface stays tight to the 15 production opcodes.

### 8.4 GAT vs `Box<dyn Iterator>` for `drain_completions`

Recommendation in IUS-7.b sec 8.4 is GAT. IUS-8.a authors the trait
with GAT; IUS-8.b implements it that way. If GAT breaks an
object-safety pattern IUR-2 needs, IUS-8.b falls back to
`Box<dyn Iterator>` and accepts the ~8 ns per-drain alloc. The
bench gate surfaces the regression.

### 8.5 Backend instance storage

IUS-7.a sec 9.1 lists three options (process-wide `OnceLock<Arc<dyn
...>>`, `Arc<dyn ...>` plumbed through `CoreConfig`, per-thread
`OnceLock<Arc<dyn ...>>`). IUS-8.a does not pick - the trait's
`&self` discipline keeps all three open. Storage decision is
downstream of IUS-8.b/c.

### 8.6 Feature-gated methods

Per IUS-7.a sec 9.5, the trait exposes methods unconditionally; the
Linux impl returns `OpcodeUnsupported` when the corresponding cargo
feature (`iouring-data-reads`, `iouring-data-writes`,
`iouring-send-zc`) is off. IUS-8.b confirms the data-reads /
data-writes paths follow the `send_zc` pattern and no caller is
forced to `#[cfg(feature = "...")]`-gate a trait method call.

### 8.7 Async-trait vs blocking dispatch

The trait methods are blocking - push to an SQ and return. The
caller polls the CQ. No `async fn submit_one`. The async story is
owned by ASY-2 / ASY-3 and sits above this trait, not inside it.

### 8.8 Deprecation of the free functions

Existing callers use `io_uring::is_io_uring_available()`,
`io_uring::writer_from_file(...)`, etc. The trait introduces method
forms but the free functions stay as shims (IUS-7.a sec 7). Open:
do they get `#[deprecated(note = "use IoUringBackend::* instead")]`?
Recommendation: not yet. Deprecation warnings churn the whole
workspace. Mark them deprecated once IUS-9 migrates the call sites.

---

**Trait method count: 57** across 5 traits (`IoUringBackend` 38,
`RingHandle` 2, `SessionPool` + `SessionLease` 3, `SharedRingHandle`
9, `DiskBatch` 5). **Path decision: A (associated types).** Path B
rejected per section 3. Linux impl ships ~600 LoC of forwarders
against ~13,665 LoC of existing wrappers; stub impl ships ~200 LoC
and deletes 2,479 LoC of mirror code. Zero-cost guarantee per
IUS-7.b sec 1.2: through-trait dispatch within 2 % of direct calls,
enforced by `tools/ci/check_zero_cost.py` on every PR that touches
`backend_impl.rs` or the trait itself.
