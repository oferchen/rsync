# IUS-8.b.1 - Linux `IoUringBackend` trait impl skeleton

Date: 2026-05-26
Scope: implementation design for `LinuxIoUringBackend` - the real Linux
impl of the `IoUringBackend` trait defined in IUS-8.a. Covers the
forwarding strategy, zero-cost preservation, migration plan, backward
compatibility, performance regression criteria, and risk areas.
Status: **SPEC DRAFT** - no source changes in this PR.
Predecessor: IUS-8.a (`docs/design/ius-8a-io-uring-backend-trait.md` -
trait surface, module placement, method-to-wrapper cross-reference).
Upstream specs: IUS-7.a (trait shape, 57 methods across 5 traits),
IUS-7.b (zero-cost guarantee, 2 % CI gate, asm-diff methodology).

---

## 0. Goal

Implement `LinuxIoUringBackend` so that every `IoUringBackend` trait
method forwards to the existing Linux io_uring wrapper code in
`crates/fast_io/src/io_uring/`. The impl must be a mechanical skeleton
of thin forwarders - no new logic, no new allocations, no new error
paths. Callers continue to use existing free functions and types; the
trait coexists alongside them. No public API visible outside `fast_io`
changes.

**Non-goals:** caller migration (IUS-9), stub collapse (IUS-8.c),
deprecation of free functions (IUS-9).

## 1. Implementation strategy

### 1.1 File layout

Two new files inside the existing `crates/fast_io/src/io_uring/`
module tree:

| File | Purpose | cfg gate |
|------|---------|----------|
| `backend.rs` | Trait definitions, error type, typed enums. Platform-free; compiles on every target. | none |
| `backend_impl.rs` | `LinuxIoUringBackend` struct + `impl IoUringBackend`. Linux-only forwarders. | `#[cfg(all(target_os = "linux", feature = "io_uring"))]` |

Both are registered in `mod.rs`:

```rust
pub mod backend;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod backend_impl;
```

`lib.rs` adds a cfg-gated re-export:

```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub use io_uring::backend_impl::LinuxIoUringBackend;
```

The existing `LinuxIoUringBackend` marker struct in `mod.rs` (lines
212-226) implements only `IoBackend` (information-only). The new struct
in `backend_impl.rs` implements `IoUringBackend` (operations). To
avoid a name collision, the new struct is named
`LinuxIoUringOpsBackend` during the coexistence window (IUS-8.b
through IUS-9). Once callers migrate to the trait (IUS-9), the old
marker struct is deleted and `LinuxIoUringOpsBackend` is renamed to
`LinuxIoUringBackend`.

**Alternative considered:** reuse the existing `LinuxIoUringBackend`
marker and add `impl IoUringBackend for LinuxIoUringBackend` in
`backend_impl.rs`. Rejected because the existing struct is
`#[derive(Clone, Copy, Default)]` with no fields; the operations
backend needs `OnceLock` fields for probe caching. Adding fields would
be a breaking change to the `Copy` impl. The rename avoids this.

### 1.2 Struct shape

```rust
pub struct LinuxIoUringOpsBackend {
    /// Cached kernel info: populated on first `is_available` or
    /// `kernel_info` call. Subsequent calls are a single pointer load.
    kernel_info: OnceLock<IoUringKernelInfo>,

    /// Cached opcode bitmap: bit N set means opcode N is supported.
    /// Populated on construction from `IORING_REGISTER_PROBE`.
    /// All 8 probe shortcut methods read from this single cache line.
    probe_cache: OnceLock<u128>,
}
```

`LinuxIoUringOpsBackend` is `Send + Sync` because `OnceLock<T>` is
`Send + Sync` when `T: Send + Sync`, and both `IoUringKernelInfo`
and `u128` satisfy those bounds.

Construction:

```rust
impl LinuxIoUringOpsBackend {
    /// Creates a new backend. Does not probe the kernel yet; probing
    /// is deferred to the first call that needs kernel info.
    pub fn new() -> Self {
        Self {
            kernel_info: OnceLock::new(),
            probe_cache: OnceLock::new(),
        }
    }
}
```

IUS-8.a section 8.2 recommends eager probe population. We follow that
recommendation by adding a `with_eager_probe()` constructor that calls
`self.kernel_info()` and `self.probe_op(Opcode::Statx)` during
construction:

```rust
pub fn with_eager_probe() -> Self {
    let backend = Self::new();
    let _ = backend.kernel_info();
    let _ = backend.probe_op(Opcode::Statx); // populates probe_cache
    backend
}
```

### 1.3 Forwarding pattern

Every trait method follows the same 1-3 line forwarding pattern. No
method body allocates, logs, validates arguments, or updates metrics
beyond what the forwarded wrapper already does. This is the IUS-7.b
section 2.5 "forwarders, not wrappers" discipline.

Example for a cold-path method:

```rust
#[inline(always)]
fn is_available(&self) -> bool {
    super::config::is_io_uring_available()
}
```

Example for the hot-path `submit_one` with match dispatch:

```rust
#[inline(always)]
fn submit_one(
    &self,
    ring: &mut Self::Ring,
    sqe: SubmissionEntry<'_>,
) -> Result<SubmissionToken, IoUringError> {
    let user_data = sqe.user_data();
    match sqe {
        SubmissionEntry::Statx { dirfd, pathname, flags, mask, statx_buf, user_data } => {
            let entry = super::statx::build_statx_sqe_unchecked(
                &super::statx::StatxArgs { dirfd, pathname, flags, mask, statx_buf },
            );
            ring.push_sqe(entry, user_data)?;
            Ok(SubmissionToken { user_data })
        }
        SubmissionEntry::Linkat { old_dirfd, old_path, new_dirfd, new_path, flags, user_data } => {
            let entry = super::linkat::build_linkat_sqe_unchecked(
                old_dirfd, old_path, new_dirfd, new_path, flags,
            );
            ring.push_sqe(entry, user_data)?;
            Ok(SubmissionToken { user_data })
        }
        // ... one arm per variant, 15 total
    }
}
```

The `_unchecked` variants of `build_*_sqe` are used because the probe
gate is the caller's responsibility (via `*_supported()` methods), not
the forwarder's. This avoids double-checking the probe cache on every
SQE.

### 1.4 Method-to-wrapper mapping (complete table)

Restating IUS-8.a section 2.1 with implementation notes:

| # | Trait method | Forwards to | Inline | Notes |
|---|---|---|---|---|
| 1 | `is_available` | `config::is_io_uring_available()` | always | Atomic load |
| 2 | `availability_reason` | `config::config_detail::io_uring_availability_reason()` | always | Allocates `String` (acceptable: cold) |
| 3 | `sqpoll_fell_back` | `config::sqpoll_fell_back()` | always | Atomic load |
| 4 | `kernel_info` | `self.kernel_info.get_or_init(...)` | always | OnceLock; delegates to `config::config_detail::io_uring_kernel_info()` |
| 5 | `build_ring` | `SharedRing::new(cfg)` or `SharedRing::try_new(...)` | always | Maps `Option` to `Result<Ring, IoUringError>` |
| 6 | `submit_one` | match dispatch per variant | always | 15 arms; see 1.3 |
| 7 | `submit_batch` | loop of `submit_one` + `submit_and_wait(0)` | always | No new code; mechanical loop |
| 8 | `submit_and_wait` | `ring.submit_and_wait(n)` | always | Direct forward |
| 9 | `drain_completions` | GAT iterator over `ring.completion()` | always | Returns concrete `DrainIter<'a>` via GAT, no Box |
| 10 | `register_buffers` | `RegisteredBufferGroup::register(...)` | always | Cold path |
| 11 | `unregister_buffers` | `RegisteredBufferGroup::unregister(...)` | always | Cold path |
| 12 | `register_files` | `SharedRing::register_files(fds)` | always | Cold path |
| 13 | `unregister_files` | `SharedRing::unregister_files()` | always | Cold path |
| 14 | `register_provided_buffer_ring` | `BufferRing::register(cfg)` | always | Cold path |
| 15 | `registered_buffer_stats` | `RegisteredBufferGroup::stats()` | always | Cold path |
| 16 | `registered_buffer_status` | `RegisteredBufferGroup::status()` | always | Cold path |
| 17 | `probe_op` | `self.probe_cache.get_or_init(...) & (1 << op)` | always | Single atomic load + bit test on warm path |
| 18-24 | `statx_supported` .. `cancel_by_fd_supported` | default impls call `probe_op` | always | Inherited from trait; no override needed except `pbuf_ring_supported` and `cancel_by_fd_supported` |
| 25 | `allocate_bgid` | `bgid_lease::with_thread_lease(\|l\| l.take())` | always | Cold path |
| 26 | `deallocate_bgid` | `BgidAllocator::deallocate(id)` | always | Cold path |
| 27 | `bgid_remaining` | `BgidAllocator::remaining()` | always | Cold path |
| 28 | `submit_statx_blocking` | `statx::submit_statx_blocking(...)` | always | Cold path |
| 29 | `submit_statx_batch` | `statx::submit_statx_batch(...)` | always | Warm path |
| 30 | `submit_linkat_blocking` | `linkat::submit_linkat_blocking(...)` | always | Cold path |
| 31 | `submit_renameat2_blocking` | `renameat2::renameat2_blocking(...)` | always | Cold path |
| 32 | `build_session_pool` | `SessionRingPool::new(cfg)` | always | Returns `Box<dyn SessionPool>` (cold) |
| 33 | `build_shared_ring` | `SharedRing::new_pair(r, w, cfg)` | always | Returns `Box<dyn SharedRingHandle>` (cold) |
| 34 | `open_reader` | `IoUringReaderFactory::open(path)` | always | Returns `Box<dyn FileReader>` (cold) |
| 35 | `open_writer` | `IoUringWriterFactory::create(path)` | always | Returns `Box<dyn FileWriter>` (cold) |
| 36 | `writer_from_file` | `super::writer_from_file(file, cap, policy)` | always | Cold path |
| 37 | `build_disk_batch` | `IoUringDiskBatch::new(cfg)` | always | Returns `Box<dyn DiskBatch>` (cold) |

### 1.5 Auxiliary trait implementations

**`RingHandle` for `SharedRing`:**

```rust
impl RingHandle for SharedRing {
    #[inline(always)]
    fn sq_entries(&self) -> u32 { self.sq_entries() }

    #[inline(always)]
    fn sqpoll_active(&self) -> bool { self.sqpoll_active() }
}
```

No newtype needed; `SharedRing` already implements `Send`.

**`SessionPool` for `SessionRingPool`:**

Thin adapter struct `SessionPoolAdapter(SessionRingPool)` implements
`SessionPool`. The `acquire` method wraps `SessionRingPool::acquire()`
into a `Box<dyn SessionLease>`.

**`SharedRingHandle` for `SharedRing`:**

Direct forwarding. Each method name matches 1:1 between the trait and
the existing `SharedRing` public methods.

**`DiskBatch` for `IoUringDiskBatch`:**

Direct forwarding. `io::Write` is already implemented on
`IoUringDiskBatch`; the trait inherits `io::Write` so the existing
impl satisfies the supertrait bound.

### 1.6 Associated type selection

```rust
impl IoUringBackend for LinuxIoUringOpsBackend {
    type Ring = SharedRing;
    // ...
}
```

`SharedRing` is the concrete ring type. Per-thread rings, session
rings, and shared rings are all configuration variants of `SharedRing`,
not separate types. This matches IUS-8.a section 3 decision (path A:
associated types).

### 1.7 GAT for `drain_completions`

```rust
type Drain<'a> = DrainIter<'a> where Self: 'a;

fn drain_completions<'a>(&self, ring: &'a mut Self::Ring) -> Self::Drain<'a> {
    DrainIter { inner: ring.completion() }
}
```

Where `DrainIter` is:

```rust
pub struct DrainIter<'a> {
    inner: io_uring::CompletionQueue<'a>,
}

impl<'a> Iterator for DrainIter<'a> {
    type Item = CompletionEntry;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|cqe| CompletionEntry {
            user_data: cqe.user_data(),
            result: cqe.result(),
            flags: cqe.flags(),
        })
    }
}
```

This avoids the per-drain `Box` allocation identified in IUS-7.b
section 6.2. The GAT form is stable since Rust 1.65; the workspace
pins 1.88.0.

## 2. Zero-cost abstraction guarantee

### 2.1 `#[inline(always)]` strategy

Every `IoUringBackend` method on `LinuxIoUringOpsBackend` carries
`#[inline(always)]`. Method bodies are 1-3 lines of forwarding code.
The cost-benefit analysis from IUS-7.b section 2.1 applies: each
inlined body is 4-16 x86 instructions; the saved `call`/`ret` pair
(~6 cycles) justifies forced inlining.

The I-cache risk is bounded because:

1. The 38 method bodies are ~300 instructions total when inlined.
2. Hot-path callers exercise 12 of the 38 methods; the remaining 26
   cold methods are called infrequently enough that I-cache locality
   is irrelevant.
3. IUS-7.b section 2.1 mandates an I-cache miss-rate check
   (`perf stat -e L1-icache-load-misses`); a >2 % regression
   triggers demotion from `#[inline(always)]` to `#[inline]` for the
   offending method.

### 2.2 Monomorphization

Hot-path callers use generic dispatch:

```rust
fn submit_loop<B: IoUringBackend>(backend: &B, ring: &mut B::Ring, ...) { ... }
```

LLVM monomorphizes `submit_loop::<LinuxIoUringOpsBackend>` and inlines
every trait method, producing assembly identical to the pre-trait
direct-call path. No vtable, no indirect call, no `Box` on the
submission hot path.

Cold-path callers may use `dyn DynIoUringBackend` (the adapter from
IUS-7.a section 9.2) for storage in long-lived state. This is
acceptable because cold methods fire once per session or once per ring
lifetime.

### 2.3 No dynamic dispatch in hot paths

The 12 hot-path methods from IUS-7.b section 5.6 must never be called
through `dyn`:

- `submit_one`, `submit_batch`, `submit_and_wait`, `drain_completions`
- `probe_op`, `statx_supported`, `linkat_supported`,
  `renameat2_supported`, `send_zc_supported`, `pbuf_ring_supported`,
  `cancel_supported`, `cancel_by_fd_supported`

The `audit_no_dyn_in_hot_path.sh` linter (IUS-7.b section 4) enforces
this by grepping for `&dyn (?:Dyn)?IoUringBackend` in hot-path
directories.

### 2.4 Expected codegen

For a call like:

```rust
backend.submit_one(&mut ring, SubmissionEntry::Statx { ... })
```

The expected x86-64 release-mode assembly is:

1. Variant tag check folds away (statically known variant).
2. `build_statx_sqe_unchecked` inlines to SQE field stores.
3. `ring.push_sqe` inlines to SQ tail-pointer bump + SQE copy.
4. `SubmissionToken` construction is a register move.

Total: same instruction count as calling
`statx::build_statx_sqe_unchecked` + `ring.push_sqe` directly.

## 3. Migration plan

### 3.1 Files changed

| File | Change type | Approx LoC delta |
|------|-------------|------------------|
| `crates/fast_io/src/io_uring/backend.rs` | NEW | +400 |
| `crates/fast_io/src/io_uring/backend_impl.rs` | NEW | +600 |
| `crates/fast_io/src/io_uring/mod.rs` | EDIT (add `pub mod backend; pub mod backend_impl;`) | +5 |
| `crates/fast_io/src/lib.rs` | EDIT (add cfg-gated re-export) | +5 |
| `crates/fast_io/tests/backend_smoke.rs` | NEW (smoke test) | +150 |
| `crates/fast_io/tests/backend_asm.rs` | NEW (asm-diff fixtures) | +80 |
| `crates/fast_io/benches/backend_dispatch.rs` | NEW (criterion harness) | +250 |
| `tools/ci/check_zero_cost.py` | NEW (CI gate script) | +80 |
| `tools/audit_no_dyn_in_hot_path.sh` | NEW (grep linter) | +40 |
| `.github/workflows/benchmarks.yml` | EDIT (add `io-uring-zero-cost` job) | +30 |

**Total: +1,640 LoC across 10 files.**

### 3.2 Files not changed

No existing wrapper file in `crates/fast_io/src/io_uring/` is edited.
The forwarders call existing public and `pub(super)` functions. If any
wrapper function needed by a forwarder is not visible, the fix is to
widen its visibility from `pub(crate)` to `pub(super)` - a purely
additive change.

No caller outside `fast_io` is changed. The trait coexists with the
existing free functions; both paths reach the same underlying code.

### 3.3 Diff shape

The diff is 100 % additive. No deletions in any existing file. New
files dominate the diff:

- `backend.rs` (trait + types): the largest file. Defines the
  five traits, error type, submission entry enum, completion entry
  struct, opcode enum. Platform-free.
- `backend_impl.rs` (Linux impl): 38 trait method bodies, each 1-10
  lines. Plus `RingHandle for SharedRing`, `SessionPool` adapter,
  `SharedRingHandle for SharedRing`, `DiskBatch for IoUringDiskBatch`.
- `backend_smoke.rs`: one function calling every trait method at least
  once; skips if `is_available()` is false.

### 3.4 Dependency impact

No new Cargo dependencies. `backend.rs` uses only `std` and types
from `io_uring_common.rs`. `backend_impl.rs` uses the existing
`io-uring` crate (already a dependency of the `io_uring` feature).

### 3.5 Ordering constraints

1. `backend.rs` must land before `backend_impl.rs` because the impl
   references the trait.
2. Both can land in a single PR (IUS-8.b) or in two stacked PRs
   (IUS-8.b.1 for trait, IUS-8.b.2 for impl). Single PR is
   preferred to avoid a window where the trait exists without an
   impl.
3. `backend_stub.rs` (IUS-8.c) depends on IUS-8.b being merged.
4. Stub tree deletion (IUS-8.c) depends on `backend_stub.rs` being
   merged and the CI cross-platform matrix passing.

## 4. Backward compatibility

### 4.1 No public API changes outside `fast_io`

The trait and impl are entirely new additions. No existing function
signature, struct, or enum is modified. Callers in `engine`,
`transfer`, `core`, `daemon`, and `cli` see zero diff.

### 4.2 Existing `LinuxIoUringBackend` marker preserved

The current `LinuxIoUringBackend` struct in `mod.rs` (implementing
`IoBackend`) stays untouched. The new struct is
`LinuxIoUringOpsBackend`. Both coexist. Callers that import
`fast_io::LinuxIoUringBackend` continue to get the marker.

### 4.3 Free functions preserved

All existing free functions (`is_io_uring_available`,
`statx_supported`, `writer_from_file`, `submit_statx_blocking`, etc.)
remain as public exports from `crate::io_uring`. They are not
deprecated in this PR (deprecation is IUS-9 scope per IUS-8.a section
8.8). Both the free-function path and the trait-method path reach the
same wrapper code.

### 4.4 Feature flag compatibility

The existing `io_uring` feature flag gates `backend_impl.rs` the same
way it gates all other Linux-only modules. No new feature flag is
introduced. `iouring-send-zc`, `iouring-data-reads`, and
`iouring-data-writes` interact with the trait as specified in IUS-7.a
section 9.5: trait methods exist unconditionally; the Linux impl
returns `IoUringError::OpcodeUnsupported` when the feature is off.

## 5. Performance regression criteria

### 5.1 Hot-path methods: 2 % CI gate

The 12 hot-path methods identified in IUS-7.b section 5.6 must pass
the criterion bench gate. For each method, the ratio
`through_trait_mean / direct_call_mean` must not exceed **1.02** on
the CI runner (x86-64 Linux, `oc-rsync-bench` container).

| Gate | Threshold | Action |
|------|-----------|--------|
| Pass | ratio <= 1.02 | Merge allowed |
| Warn | 1.02 < ratio <= 1.05 | Merge allowed with follow-up issue |
| Fail | ratio > 1.05 | Merge blocked |

### 5.2 Asm-diff gate

For each of the 12 hot-path methods, the normalised assembly diff
between `submit_one_through_trait` and `submit_one_direct` must be
empty. Normalisation strips register names and function
prologue/epilogue stitching. Non-empty normalised diff blocks merge.

### 5.3 End-to-end benchmark

The existing daemon cold-start benchmark (DIS-8.a workflow) and the
SSH transfer benchmark serve as regression guards. They do not directly
measure through-trait overhead (they measure wall-clock time including
kernel I/O), but a >5 % wall-clock regression on either would indicate
the trait indirection is leaking into the real workload and must be
investigated.

### 5.4 What is measured

The `backend_dispatch` criterion bench measures pure dispatch overhead:

- Uses `IORING_OP_NOP` (added to `SubmissionEntry` behind
  `#[cfg(any(test, bench))]`) to isolate userspace dispatch cost from
  kernel I/O.
- 10 M iterations per bench arm.
- Two arms per hot-path method: `_through_trait` (generic dispatch
  through `B: IoUringBackend`) and `_direct` (calls the underlying
  wrapper function directly).
- Runs on bare-metal-equivalent CI (dedicated self-hosted runner, no
  co-tenancy).

### 5.5 What is NOT measured

- Kernel-side `io_uring_enter` cost (unchanged by the trait).
- Cross-crate LTO effects (the bench runs within `fast_io`).
- `dyn` dispatch cost (cold-path only; not benchmarked).
- `Arc<dyn>` overhead (prohibited on hot paths by linter).

## 6. Risk areas

### 6.1 `submit_one` match dispatch overhead

The `SubmissionEntry` enum has 15 variants. On the pre-trait path,
each opcode's SQE is built directly by calling the wrapper. On the
trait path, the SQE goes through a `match` in `submit_one`.

**Risk:** when the variant is not statically known at the call site
(e.g., a `Vec<SubmissionEntry>` of mixed opcodes), LLVM emits a
jump table instead of folding the match. The jump table adds ~2-4 ns
per SQE dispatch (branch prediction miss on variant transitions).

**Mitigation:** the existing codebase always submits SQEs of known
type at the call site (e.g., `build_statx_sqe()` is always followed
by a statx submission, never mixed with other opcodes in a single
submit loop). The match tag folds away. If a future caller submits
heterogeneous SQEs through `submit_batch`, the jump-table cost is
bounded by the number of variant transitions per batch - typically
zero because batches are opcode-homogeneous.

**Verification:** the asm-diff fixture for `submit_one` uses a
statically-known variant (`Statx`). The bench also measures the
heterogeneous case (mixed opcodes in a vector) to establish the
jump-table baseline - this baseline is informational, not gated.

### 6.2 `drain_completions` GAT ergonomics

The GAT form (`type Drain<'a>: Iterator<...>`) is zero-cost but
complicates the `dyn` adapter. `DynIoUringBackend` must provide a
boxed form (`Box<dyn Iterator>`) for callers that need trait-object
storage. Two iterator types coexist: `DrainIter<'a>` (concrete, via
GAT) and `Box<dyn Iterator<Item = CompletionEntry> + 'a>` (erased,
via `DynIoUringBackend`).

**Risk:** a caller accidentally uses the `dyn` path on the hot loop,
paying a per-drain `Box` allocation (~8 ns).

**Mitigation:** the `audit_no_dyn_in_hot_path.sh` linter catches
`&dyn` usage in hot-path directories. The bench gate catches the
run-time regression.

### 6.3 `OnceLock` probe cache contention

The `probe_cache: OnceLock<u128>` is shared across threads. On first
access, `OnceLock::get_or_init` takes an internal mutex. Subsequent
accesses are a single atomic `Acquire` load.

**Risk:** if multiple threads race to populate the cache during
startup, the mutex serializes them. Startup cost is bounded (7
`IORING_REGISTER_PROBE` syscalls) and one-shot.

**Mitigation:** the `with_eager_probe()` constructor populates the
cache at construction time, before any thread forks. The steady-state
hot path is a single atomic load - zero contention.

### 6.4 SharedRing as the associated type

`type Ring = SharedRing` fixes the ring type. If a future optimization
needs a different ring shape (e.g., a lightweight ring without
registered buffers for metadata-only operations), it cannot be
expressed as a different `Ring` type without changing the associated
type.

**Risk:** low. The current codebase uses `SharedRing` for all ring
operations. Per-thread rings (`PerThreadRing`) are a configuration of
the underlying `io-uring` crate's `IoUring`, not a separate ring
wrapper. If a truly different ring type emerges, it can be added as a
second backend impl (e.g., `LinuxLightweightBackend`) rather than
changing the associated type on the primary backend.

**Mitigation:** none needed now. The IUR-2 per-thread rings work will
validate whether `SharedRing` is sufficient or a second impl is
needed.

### 6.5 `push_sqe` visibility

`SharedRing::push_sqe` may be `pub(crate)` or `pub(super)` today. The
`submit_one` forwarder in `backend_impl.rs` calls it. If the method
is not visible from the `backend_impl` module, it must be widened to
`pub(super)`.

**Risk:** widening visibility could expose an internal method to
unintended callers within the `io_uring` module tree.

**Mitigation:** `pub(super)` is the minimum required visibility. It
does not expose the method outside `crate::io_uring`. If further
restriction is needed, a `pub(in crate::io_uring)` path-based
visibility annotation can be used.

### 6.6 Error type mapping

The existing wrappers return `io::Result<T>`. The trait returns
`Result<T, IoUringError>`. The forwarders must map `io::Error` to
`IoUringError::IoError(e)` on the error path.

**Risk:** the error mapping adds one branch per fallible call. On
the happy path (no error), the branch is predicted-not-taken and costs
zero cycles.

**Mitigation:** the mapping is a single `map_err(IoUringError::IoError)`
call, which LLVM folds into the error-path branch of the callee. No
allocation, no formatting, no string construction on the happy path.

### 6.7 Feature-gated opcodes returning `OpcodeUnsupported`

When `iouring-send-zc` is disabled, `submit_one(SendZc { ... })`
returns `Err(IoUringError::OpcodeUnsupported { opcode: Opcode::SendZc })`.
The caller must handle this error variant.

**Risk:** a caller that does not expect `OpcodeUnsupported` may
propagate it as an unhandled error, aborting the transfer.

**Mitigation:** the existing callers of `try_send_zc` already handle
the unsupported case by falling back to `IORING_OP_SEND`. The trait
method returns the same error in the same circumstances; the caller
logic is unchanged.

## 7. Implementation checklist (IUS-8.b reviewer gate)

Restating IUS-8.a section 4 with implementation-specific items:

- [ ] `LinuxIoUringOpsBackend` struct has `OnceLock<IoUringKernelInfo>`
      and `OnceLock<u128>` fields only. No `Arc`, no `Mutex`, no
      additional state.
- [ ] Every method body carries `#[inline(always)]`.
- [ ] Every method body is 1-10 lines. No method body exceeds 10 LoC
      (excluding the `submit_one` match block, which is 15 arms of
      ~3 lines each = ~45 LoC total).
- [ ] No method body contains `log::*`, `tracing::*`, `format!`,
      `String::from`, `Vec::new`, `Box::new`, or any other allocation.
- [ ] `probe_op` reads from `OnceLock<u128>` with a single atomic
      load + bit test. No branch on cold/warm after init.
- [ ] `drain_completions` returns a GAT-typed `DrainIter<'a>`, not
      `Box<dyn Iterator>`.
- [ ] `submit_one` uses `_unchecked` SQE builders (no double probe).
- [ ] `backend_smoke.rs` covers all 57 trait methods.
- [ ] `backend_asm.rs` produces empty normalised diff for 12 hot-path
      methods.
- [ ] `backend_dispatch.rs` reports through-trait / direct ratio
      <= 1.02 for all hot-path methods.
- [ ] No existing file in `crates/fast_io/src/io_uring/` is modified
      beyond `mod.rs` (add two `pub mod` lines).
- [ ] Cross-platform CI passes: Linux, macOS, Windows. The new files
      compile on all targets (trait definitions in `backend.rs` are
      platform-free; `backend_impl.rs` is `cfg`-gated out on
      non-Linux).

## 8. Open questions

### 8.1 Naming: `LinuxIoUringOpsBackend` vs immediate rename

Two paths:

1. **Coexistence name** (`LinuxIoUringOpsBackend`): avoids touching
   the existing `LinuxIoUringBackend` marker. Requires a rename in
   IUS-9 when the marker is deleted.
2. **Immediate rename**: rename the existing marker to
   `LinuxIoUringInfoBackend` (or delete it) and use
   `LinuxIoUringBackend` for the operations backend now.

Recommendation: option 1. The marker struct has downstream callers
that import `fast_io::LinuxIoUringBackend`. Changing it in IUS-8.b
is unnecessary churn. The rename in IUS-9 is a single `sed` command
plus a clippy-clean pass.

### 8.2 `SharedRing::push_sqe` signature

If `push_sqe` takes `io_uring::squeue::Entry` (the `io-uring` crate's
SQE type), the forwarder can pass the built SQE directly. If it takes
a different shape, the forwarder must adapt. This is a minor
implementation detail resolved during coding.

### 8.3 `submit_batch` optimality

The current spec says `submit_batch` is a loop of `submit_one` +
`submit_and_wait(0)`. A more efficient form would push all SQEs to
the SQ without intermediate `submit_and_wait` calls, then issue one
`io_uring_enter` at the end. The `SharedRing` API may or may not
expose a push-without-enter primitive.

Recommendation: implement the loop form first. If the bench shows
`submit_batch` exceeding the 2 % gate, the optimization to a
single-enter form is straightforward and does not change the trait
surface.

### 8.4 `backend_asm.rs` on CI without `cargo-show-asm`

The asm-diff CI step requires `cargo-show-asm` (or `cargo asm`). If
the CI runner does not have it pre-installed, the step must
`cargo install cargo-show-asm` on every run (~30 s).

Recommendation: pre-install `cargo-show-asm` in the `oc-rsync-bench`
container image. The install cost is one-time.

---

**Summary:** the Linux `IoUringBackend` impl is a ~600 LoC file of
thin forwarders, each marked `#[inline(always)]`, each 1-10 lines,
each calling the existing wrapper code unchanged. No new logic, no new
allocations, no new error paths. The 2 % CI gate and asm-diff verify
zero-cost. No public API changes outside `fast_io`. The trait coexists
with existing free functions until IUS-9 migrates callers.
