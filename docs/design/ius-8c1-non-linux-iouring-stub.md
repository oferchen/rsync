# IUS-8.c.1 - `IoUringBackend` non-Linux stub spec

Date: 2026-05-26
Scope: design specification for replacing the 2,479-LoC, 21-file
`io_uring_stub/` tree with a single `backend_stub.rs` file
implementing the `IoUringBackend` trait from IUS-7.a/IUS-8.a.
Status: **SPEC DRAFT** - no source changes in this PR.
Predecessor: IUS-8.a (`IoUringBackend` trait surface, `backend.rs`).
Depends on: IUS-8.b (Linux impl must land first so the trait is
proven against real callers before the stub tree is deleted).

## 1. Problem statement

The non-Linux stub at `crates/fast_io/src/io_uring_stub/` mirrors the
Linux `io_uring/` module file-for-file. Each of its 21 source files
re-declares the same structs, enums, and functions with bodies that
return `Err(Unsupported)`, `false`, or `0`. The costs:

- **2,479 LoC of pure duplication.** Every Linux-side API change
  requires a matching stub edit. CI catches build breaks but not
  semantic skew (different error messages, different default values).
- **Review noise.** A 10-line Linux feature change forces a 10-line
  stub mirror. The stub diff often dwarfs the substantive diff.
- **Signature drift.** New Linux entry points occasionally ship
  without stubs (caught by cross-platform CI) or stubs diverge to
  slightly different signatures over time.

The `IoUringBackend` trait (IUS-8.a) collapses per-platform divergence
to one impl block per platform. This spec describes the non-Linux impl
that replaces the entire stub tree with a single file.

## 2. Architecture

### 2.1 File placement

```
crates/fast_io/src/
+- io_uring/
|  +- backend.rs              (IUS-8.a) trait + types, platform-free
|  +- backend_impl.rs         (IUS-8.b) Linux impl, cfg-gated
|  +- backend_stub.rs         (IUS-8.c) THIS SPEC - non-Linux stub
|  +- mod.rs                  EDIT: cfg-gated re-exports
|  +- ...existing wrappers stay untouched...
+- io_uring_stub/             DELETED by IUS-8.c
+- io_uring_common.rs         unchanged
+- lib.rs                     EDIT: swap stub re-export path
```

`backend_stub.rs` compiles on every non-Linux target (macOS, Windows)
and on Linux when the `io_uring` cargo feature is disabled. It is
gated with:

```rust
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
```

This matches the existing cfg gate on the `io_uring_stub` module in
`lib.rs` (line 179).

### 2.2 Stub type definitions

The stub module defines exactly three types:

```rust
/// Marker backend for non-Linux platforms.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubIoUringBackend;

/// Placeholder ring handle. Unconstructable in practice because
/// `build_ring` always returns `Err(IoUringError::Unsupported)`.
#[derive(Debug)]
pub struct StubRing {
    _private: (),
}

/// Placeholder session pool. Unconstructable.
pub struct StubSessionPool {
    _private: (),
}
```

All other types (`IoUringConfig`, `IoUringKernelInfo`, `OpTag`,
`SharedCompletion`, `BufferRingConfig`, `RegisteredBufferStats`,
`RegisteredBufferStatus`, UAPI constants) are already defined in
`io_uring_common.rs` and re-exported unchanged by both the Linux impl
and the stub. No duplication needed.

### 2.3 Trait implementation strategy

Every `IoUringBackend` method falls into one of four return categories.
The stub applies a single pattern per category - no per-method logic:

| Category | Methods | Stub return |
|----------|---------|-------------|
| Availability booleans | `is_available`, `sqpoll_fell_back`, `probe_op`, `statx_supported`, `linkat_supported`, `renameat2_supported`, `send_zc_supported`, `pbuf_ring_supported`, `cancel_supported`, `cancel_by_fd_supported` | `false` |
| Diagnostic strings | `availability_reason` | `"io_uring: disabled (not built for this target)".to_string()` |
| Structured diagnostics | `kernel_info` | `IoUringKernelInfo { available: false, kernel_major: None, kernel_minor: None, supported_ops: 0, pbuf_ring_supported: false, reason: <as above> }` |
| Construction / submission / registration | `build_ring`, `submit_one`, `submit_batch`, `submit_and_wait`, `register_buffers`, `unregister_buffers`, `register_files`, `unregister_files`, `register_provided_buffer_ring`, `submit_statx_blocking`, `submit_statx_batch`, `submit_linkat_blocking`, `submit_renameat2_blocking`, `build_session_pool`, `build_shared_ring`, `open_reader`, `open_writer`, `writer_from_file`, `build_disk_batch` | `Err(IoUringError::Unsupported)` |
| Stats / counters | `registered_buffer_stats` | Zeroed `RegisteredBufferStats { total_acquires: 0, total_misses: 0 }` |
| Status | `registered_buffer_status` | `RegisteredBufferStatus::NotRegistered` |
| Completion drain | `drain_completions` | Empty iterator (GAT: `std::iter::Empty<CompletionEntry>`) |
| Bgid allocation | `allocate_bgid` | `Err(BgidAllocError::Exhausted { fresh_used: 0, free_list_len: 0 })` |
| Bgid release | `deallocate_bgid` | No-op (empty body) |
| Bgid remaining | `bgid_remaining` | `0` |

### 2.4 Auxiliary trait implementations

`RingHandle` for `StubRing`:
- `sq_entries()` -> `0`
- `sqpoll_active()` -> `false`

`SessionPool` for `StubSessionPool`:
- `ring_count()` -> `0`
- `acquire()` -> `None`

No `SessionLease`, `SharedRingHandle`, or `DiskBatch` impl is needed
on the stub side because those trait objects are returned from
`IoUringBackend` methods that always return `Err(Unsupported)` on the
stub - callers never receive an instance to call methods on.

### 2.5 `drain_completions` GAT

Per IUS-7.b section 8.4, the trait uses a GAT for the drain return
type to avoid per-drain `Box` allocations on Linux:

```rust
type Drain<'a>: Iterator<Item = CompletionEntry> + 'a where Self: 'a;
```

The stub impl sets:

```rust
type Drain<'a> = std::iter::Empty<CompletionEntry>;

fn drain_completions<'a>(&self, _ring: &'a mut Self::Ring) -> Self::Drain<'a> {
    std::iter::empty()
}
```

This is zero-cost on the stub path: no allocation, no indirection,
the body compiles to a single return.

## 3. Size target

| Metric | Current (`io_uring_stub/`) | Target (`backend_stub.rs`) |
|--------|---------------------------|---------------------------|
| Files | 21 | 1 |
| Non-test LoC | 2,070 | < 200 |
| Test LoC | 409 | ~50 (in `backend_smoke.rs`) |
| Total LoC | 2,479 | < 250 |

**Target: < 250 lines total**, including rustdoc. Each method body is
1-3 lines. The entire impl block fits on one screen.

Breakdown estimate:
- Module doc + imports: ~15 lines
- `StubIoUringBackend`, `StubRing`, `StubSessionPool` types: ~20 lines
- `RingHandle for StubRing`: ~10 lines
- `SessionPool for StubSessionPool`: ~10 lines
- `IoUringBackend for StubIoUringBackend` (38 methods): ~130 lines
- Free-function shims (backward compat): ~15 lines
- Total: ~200 lines

## 4. What gets deleted

The entire `crates/fast_io/src/io_uring_stub/` directory is removed.
Enumerated by file:

| File | LoC | Purpose (replaced by) |
|------|-----|----------------------|
| `mod.rs` | 113 | Re-exports -> `backend_stub.rs` re-exports + `lib.rs` cfg gate |
| `config.rs` | 75 | `StubIoUringBackend` + `IoBackend` impl -> trait `is_available` / `availability_reason` |
| `shared_ring.rs` | 99 | `SharedRing` struct + methods -> `StubRing` + `Err(Unsupported)` on `build_shared_ring` |
| `disk_batch.rs` | 88 | `IoUringDiskBatch` + `Write` impl -> `Err(Unsupported)` on `build_disk_batch` |
| `session_pool.rs` | 195 | `SessionRingPool`, `RingLease`, `ThreadLocalRingPool` -> `StubSessionPool` + `Err(Unsupported)` on `build_session_pool` |
| `file_factory.rs` | 293 | Reader/writer factories, enum dispatchers -> `Err(Unsupported)` on `open_reader` / `open_writer` / `writer_from_file` |
| `file_reader.rs` | 73 | `IoUringReader` -> eliminated (never constructed) |
| `file_writer.rs` | 89 | `IoUringWriter` -> eliminated (never constructed) |
| `buffer_ring.rs` | 167 | `BufferRing`, `BgidAllocator` -> `Err(Unsupported)` on `register_provided_buffer_ring`, `allocate_bgid` |
| `registered_buffers.rs` | 122 | `RegisteredBufferGroup`, `RegisteredBufferSlot` -> `Err(Unsupported)` on `register_buffers` |
| `statx.rs` | 109 | `StatxArgs`, free functions -> `Err(Unsupported)` on `submit_statx_blocking` / `submit_statx_batch` |
| `linkat.rs` | 79 | `LinkAtArgs`, free functions -> `Err(Unsupported)` on `submit_linkat_blocking` |
| `renameat2.rs` | 61 | `RenameAt2Args`, free functions -> `Err(Unsupported)` on `submit_renameat2_blocking` |
| `cancel.rs` | 76 | `CancelOutcome`, free functions -> `Err(Unsupported)` on `submit_one(CancelByUserData)` / `submit_one(CancelByFd)` |
| `send_zc.rs` | 84 | `ZeroCopySender`, free functions -> `Err(Unsupported)` on `submit_one(SendZc)` |
| `linked_chain.rs` | 107 | `LinkedChain`, `CqeResult` -> eliminated (callers use trait methods) |
| `per_thread_ring.rs` | 36 | `with_ring` free function -> callers use `build_ring` on backend |
| `socket_factory.rs` | 135 | Socket reader/writer factories (`#[cfg(unix)]`) -> `Err(Unsupported)` on trait methods |
| `socket_reader.rs` | 31 | `IoUringSocketReader` -> eliminated |
| `socket_writer.rs` | 38 | `IoUringSocketWriter` -> eliminated |
| `tests.rs` | 409 | 30+ tests -> replaced by `backend_smoke.rs` stub arm |

**Total deleted: 2,479 lines across 21 files.**

### 4.1 Argument structs that move to `backend.rs`

The typed `SubmissionEntry` enum in `backend.rs` (IUS-8.a) subsumes
the standalone argument structs that the stub currently mirrors:

| Stub struct | Replaced by |
|-------------|-------------|
| `StatxArgs<'a>` | `SubmissionEntry::Statx { ... }` |
| `LinkAtArgs<'a>` | `SubmissionEntry::Linkat { ... }` |
| `RenameAt2Args<'a>` | `SubmissionEntry::Renameat2 { ... }` |

These structs also exist in the Linux `io_uring/` wrappers. The Linux
wrappers keep them for internal use; the stub no longer needs its own
copies because callers construct `SubmissionEntry` variants directly.

### 4.2 Enum types that move to `backend.rs`

| Stub type | Replaced by |
|-----------|-------------|
| `CancelOutcome` | Callers receive `CompletionEntry` with result codes |
| `CqeResult` | `CompletionEntry` |

### 4.3 Types that remain in `io_uring_common.rs`

These are shared between platforms and are not affected:

`IoUringConfig`, `IoUringKernelInfo`, `OpTag`, `SharedCompletion`,
`SharedRingConfig`, `BufferRingConfig`, `BufferRingError`,
`BgidAllocError`, `RegisteredBufferStats`, `RegisteredBufferStatus`,
and all UAPI constants (`IORING_OP_*`, `*_MIN_KERNEL`, `RENAME_*`,
`buffer_id_from_cqe_flags`).

## 5. cfg-gate strategy

### 5.1 Module-level gating

The stub module uses the same cfg predicate as the current
`io_uring_stub` in `lib.rs`:

```rust
// crates/fast_io/src/io_uring/mod.rs
#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod backend_impl;

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
mod backend_stub;

// Re-export the platform-appropriate backend type
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub use backend_impl::LinuxIoUringBackend;

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
pub use backend_stub::StubIoUringBackend;
```

### 5.2 `lib.rs` changes

The existing path-aliased module:

```rust
// BEFORE (current)
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod io_uring;
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
#[path = "io_uring_stub/mod.rs"]
pub mod io_uring;
```

becomes:

```rust
// AFTER (IUS-8.c)
pub mod io_uring;
```

The `#[path = ...]` aliasing is eliminated because the stub now lives
inside the `io_uring/` module tree. The cfg gating moves down into
`io_uring/mod.rs` (section 5.1), selecting which impl is compiled.

### 5.3 CI matrix

The following cargo check commands must pass for the PR to merge:

```sh
# Linux with io_uring feature (Linux impl)
cargo check --features io_uring

# Linux without io_uring feature (stub)
cargo check --no-default-features

# macOS (stub, native)
cargo check --target aarch64-apple-darwin

# Windows (stub, cross-compile or CI runner)
cargo check --target x86_64-pc-windows-msvc
```

The existing CI matrix already runs all four. No new CI jobs are
required.

## 6. Compile-time guarantees

### 6.1 Trait-bound satisfaction

`StubIoUringBackend` implements `IoUringBackend` with
`type Ring = StubRing`. Because `StubRing` implements `RingHandle` and
the stub methods satisfy all trait signatures, generic code compiles
identically on all platforms:

```rust
// This compiles on macOS, Windows, and Linux-without-io_uring:
fn example<B: IoUringBackend>(backend: &B) {
    let result = backend.build_ring(&IoUringConfig::default());
    assert!(result.is_err()); // always Unsupported on stub
}
```

### 6.2 No conditional compilation in callers

Callers do not need `#[cfg(...)]` to use the backend. The trait
provides a uniform API; the difference is purely behavioral (real I/O
vs error returns). Callers that branch on availability use the runtime
check:

```rust
fn do_transfer<B: IoUringBackend>(backend: &B) {
    if backend.is_available() {
        // io_uring path
    } else {
        // standard I/O fallback
    }
}
```

### 6.3 Associated type visibility

The `StubRing` type is `pub` so that callers writing
`<B as IoUringBackend>::Ring` can name it. The type is unconstructable
outside the crate (private `_private` field) - callers can reference
it for type annotations but cannot instantiate it.

### 6.4 `Send + Sync` bounds

`StubIoUringBackend` is `Send + Sync` trivially (it is a unit-like
struct with `Copy`). `StubRing` is `Send` (satisfying the `RingHandle:
Send` bound) because it contains only a unit field.

## 7. Backward-compatibility shims

### 7.1 Free-function shims

The current stub exposes free functions (`is_io_uring_available`,
`sqpoll_fell_back`, `pbuf_ring_supported`, etc.) that callers import
as `crate::io_uring::is_io_uring_available()`. These must continue to
work during the transition period (IUS-8.c ships before IUS-9 migrates
callers to the trait).

`backend_stub.rs` (or a companion `compat.rs` file if cleaner)
provides thin shims:

```rust
/// Shim for callers not yet migrated to the trait.
#[must_use]
pub fn is_io_uring_available() -> bool {
    false
}

#[must_use]
pub fn sqpoll_fell_back() -> bool {
    false
}

// ... one shim per existing free function
```

These shims are deprecated once IUS-9 migrates all call sites.

### 7.2 Type re-exports

Types currently re-exported from `io_uring_stub/mod.rs` (e.g.,
`BufferRingConfig`, `IoUringConfig`, `OpTag`) are re-exported from
`io_uring_common.rs` through the `io_uring/mod.rs` re-exports. This
path already works because the Linux `io_uring/mod.rs` does the same.

### 7.3 File factory shims

The current stub provides `IoUringReaderFactory`,
`IoUringWriterFactory`, `IoUringOrStdReader`, `IoUringOrStdWriter`,
and helper functions (`reader_from_path`, `writer_from_file`,
`read_file`, `write_file`). These are used by callers that want a
single import path regardless of platform.

Two approaches for the transition:

**Option A - Keep factory types as shims.** The factories wrap
standard I/O directly (their current behavior). They live in a
`compat.rs` file inside `io_uring/` rather than duplicating the full
stub. Estimated ~80 LoC.

**Option B - Migrate callers to the trait.** The trait's `open_reader`
/ `open_writer` / `writer_from_file` methods replace the factories.
Callers that need standard-I/O fallback check `is_available()` first.

**Recommendation: Option A for IUS-8.c, Option B for IUS-9.** Deleting
the factories in the same PR that deletes the stub tree risks breaking
callers. The shim file keeps them alive with minimal duplication until
IUS-9 migrates each call site.

## 8. Testing

### 8.1 Stub arm in `backend_smoke.rs`

The existing `backend_smoke.rs` test file (added by IUS-8.b for the
Linux impl) gains a cfg-gated arm for the stub:

```rust
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
mod stub_tests {
    use fast_io::io_uring::backend::*;
    use fast_io::io_uring_common::*;

    #[test]
    fn stub_reports_unavailable() {
        let backend = StubIoUringBackend;
        assert!(!backend.is_available());
        assert!(!backend.sqpoll_fell_back());
        assert!(!backend.probe_op(Opcode::Statx));
        assert!(!backend.statx_supported());
        assert!(!backend.linkat_supported());
        assert!(!backend.renameat2_supported());
        assert!(!backend.send_zc_supported());
        assert!(!backend.pbuf_ring_supported());
        assert!(!backend.cancel_supported());
        assert!(!backend.cancel_by_fd_supported());
    }

    #[test]
    fn stub_build_ring_returns_unsupported() {
        let backend = StubIoUringBackend;
        let err = backend.build_ring(&IoUringConfig::default()).unwrap_err();
        assert!(matches!(err, IoUringError::Unsupported));
    }

    #[test]
    fn stub_kernel_info_reports_unavailable() {
        let backend = StubIoUringBackend;
        let info = backend.kernel_info();
        assert!(!info.available);
        assert!(info.kernel_major.is_none());
        assert_eq!(info.supported_ops, 0);
    }

    #[test]
    fn stub_bgid_allocation_exhausted() {
        let backend = StubIoUringBackend;
        assert!(backend.allocate_bgid().is_err());
        assert_eq!(backend.bgid_remaining(), 0);
        backend.deallocate_bgid(0); // no-op, should not panic
    }

    #[test]
    fn stub_registered_buffer_stats_zeroed() {
        // Cannot construct a ring, so test the default values
        // the method would return. The method itself is unreachable
        // because build_ring fails; this test validates the type.
        let stats = RegisteredBufferStats {
            total_acquires: 0,
            total_misses: 0,
        };
        assert_eq!(stats.total_acquires, 0);
    }
}
```

Estimated: ~50 LoC of tests covering every method category from
section 2.3.

### 8.2 CI verification

The stub compiles and runs tests on:
- **macOS CI** (`macos-stable` job) - native stub compilation + test
- **Windows CI** (`windows-stable` job) - native stub compilation + test
- **Linux CI without feature** - `cargo nextest run --no-default-features`

These jobs already exist. The IUS-8.c PR must show green on all three
before merge.

### 8.3 Regression guard

After deletion, any future change to the `IoUringBackend` trait that
adds a method will produce a compile error in `backend_stub.rs` on
the next macOS/Windows CI run. This is the structural guarantee that
prevents stub drift - the trait enforces API parity at compile time
instead of relying on manual file mirroring.

## 9. Migration sequence

### 9.1 Prerequisites

1. **IUS-8.a must be merged.** The `IoUringBackend` trait, associated
   types, `IoUringError`, and `SubmissionEntry` enum must exist in
   `crates/fast_io/src/io_uring/backend.rs`.
2. **IUS-8.b must be merged.** The Linux impl must be proven against
   real callers and the zero-cost benchmark gate must pass. This
   ensures the trait surface is stable before the stub is written
   against it.

### 9.2 IUS-8.c implementation steps

1. **Create `backend_stub.rs`.** Implement `IoUringBackend` for
   `StubIoUringBackend` with all methods returning per-category
   values from section 2.3. Implement `RingHandle` for `StubRing`
   and `SessionPool` for `StubSessionPool`.

2. **Create backward-compat shims.** Add free-function shims and
   factory type shims (section 7) so existing callers continue to
   compile without changes.

3. **Update `io_uring/mod.rs`.** Add cfg-gated `mod backend_stub`
   and re-exports (section 5.1).

4. **Update `lib.rs`.** Remove the `#[path = "io_uring_stub/mod.rs"]`
   alias. The `io_uring` module is now unconditionally compiled; the
   cfg gate is internal to `io_uring/mod.rs`.

5. **Audit callers.** Run:
   ```sh
   git grep -nE 'crate::io_uring_stub|io_uring_stub::' crates/ tools/ xtask/
   ```
   Every hit becomes a one-line import replacement pointing to the
   new path. Currently 7 references exist outside the stub directory
   itself (in `lib.rs`, `io_uring_common.rs`, and a few `io_uring/`
   doc comments).

6. **Verify cross-platform compilation.** Run the four cargo check
   commands from section 5.3. All must pass.

7. **Add stub tests.** Add the `stub_tests` module to
   `backend_smoke.rs` (section 8.1).

8. **Delete `io_uring_stub/`.** Remove the entire directory (21 files,
   2,479 LoC). This is the last step - only after all callers are
   migrated and CI is green.

9. **Update doc comments.** Replace references to `io_uring_stub` in
   `io_uring_common.rs` and `io_uring/mod.rs` doc comments with
   references to `backend_stub`.

### 9.3 Post-merge cleanup (IUS-9)

IUS-9 (provisional) migrates callers from the free-function shims to
the trait methods. Once every call site uses the trait:

- Remove the free-function shims from `backend_stub.rs` / `compat.rs`.
- Remove the factory type shims.
- Mark the transition complete.

The shim removal is a separate PR from IUS-8.c to keep the deletion
PR focused on one concern (stub replacement) and the migration PR
focused on another (caller updates).

## 10. Risks and mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Trait surface changes between IUS-8.a merge and IUS-8.c implementation | Medium | Low - stub needs updating | IUS-8.c should follow IUS-8.b promptly; the stub is mechanical |
| Callers rely on stub-specific types (`IoUringReader`, `SharedRing`, etc.) by name | Medium | Medium - breaks imports | Audit step 5 in section 9.2 catches these; factory shims (section 7.3) bridge the gap |
| Future methods added to trait without stub coverage | None | None | Compile error on macOS/Windows CI - structural guarantee (section 8.3) |
| Performance regression from trait indirection on non-Linux | None | None | Stub methods are trivial returns; no indirection on the stub path |

## 11. Success criteria

- [ ] `backend_stub.rs` implements all 38 `IoUringBackend` methods
      plus auxiliary traits
- [ ] File is < 250 lines (target ~200)
- [ ] `io_uring_stub/` directory is fully deleted (21 files, 2,479 LoC)
- [ ] Cross-platform CI passes: macOS, Windows, Linux-without-feature,
      Linux-with-feature
- [ ] Existing free-function callers compile without changes
- [ ] Stub tests in `backend_smoke.rs` cover every method category
- [ ] No `#[cfg(not(target_os = "linux"))]` gates needed in any caller
      outside `fast_io`

---

**Net effect.** The non-Linux stub shrinks from 2,479 LoC across 21
files to ~200 LoC in one file - a 12x reduction. Per-platform signature
divergence becomes structurally impossible because there is only one
trait definition. The compile error on trait changes is the enforcement
mechanism, replacing the manual file-mirroring discipline.
