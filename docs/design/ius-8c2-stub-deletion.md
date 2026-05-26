# IUS-8.c.2 - Delete `io_uring_stub/` after trait migration lands

Date: 2026-05-26
Scope: implementation plan for deleting the legacy `io_uring_stub/`
directory tree once IUS-8.c.1 (`backend_stub.rs`) and IUS-8.b.2 (caller
migration) have landed. This is the final step in the IUS-7/8 series
that eliminates the 2,479-LoC, 21-file stub mirror.
Status: **SPEC DRAFT** - no source changes in this PR.
Predecessor: IUS-8.c.1 (`StubIoUringBackend` impl in `backend_stub.rs`,
merged), IUS-8.b.2 (caller migration to `IoUringBackend` trait, PR
#5015, merged).
Related: IUS-8.a (trait surface), IUS-8.b.1 (Linux impl), IUS-7.a
(trait shape), IUS-7.b (zero-cost guarantee).

---

## 0. Goal

Delete `crates/fast_io/src/io_uring_stub/` (21 files, 2,479 LoC) and
all residual references to it. After this change:

- The `io_uring` module compiles unconditionally on every platform.
- Non-Linux platforms use `backend_stub.rs` inside `io_uring/` - not a
  separate path-aliased module tree.
- Doc comments, `lib.rs` cfg gates, and cross-references no longer
  mention `io_uring_stub`.
- Net reduction: ~2,300 LoC deleted (2,479 stub LoC minus ~180 LoC of
  shim code that may still live in `backend_stub.rs` or `compat.rs`).

**Non-goals:** deprecating free-function shims (IUS-9), adding new
io_uring features, changing any behavioral semantics.

## 1. Pre-deletion checklist

Every item must be verified green before the deletion PR is opened.

### 1.1 Upstream prerequisites merged

- [ ] **IUS-8.a** - `IoUringBackend` trait and associated types exist in
      `crates/fast_io/src/io_uring/backend.rs`.
- [ ] **IUS-8.b.1** - `LinuxIoUringOpsBackend` impl exists in
      `crates/fast_io/src/io_uring/backend_impl.rs`, passing the 2% CI
      zero-cost gate and asm-diff fixture.
- [ ] **IUS-8.b.2** - All 17 caller sites migrated to route through the
      trait (type-alias approach via `PlatformIoUringBackend`). PR #5015
      merged.
- [ ] **IUS-8.c.1** - `StubIoUringBackend` impl exists in
      `crates/fast_io/src/io_uring/backend_stub.rs`, compiled on
      non-Linux and Linux-without-feature.

### 1.2 Caller audit

Run the following grep to confirm zero external references remain:

```sh
git grep -nE 'crate::io_uring_stub|io_uring_stub::' crates/ tools/ xtask/
```

Expected: hits only inside `crates/fast_io/src/io_uring_stub/` itself
(to be deleted), plus doc-comment references in:

- `crates/fast_io/src/io_uring_common.rs` (3 doc references)
- `crates/fast_io/src/io_uring/mod.rs` (1 doc reference)
- `crates/fast_io/src/io_uring/buffer_ring/mod.rs` (1 doc reference)
- `crates/fast_io/src/io_uring/renameat2.rs` (1 doc reference)
- `crates/fast_io/src/lib.rs` (1 `#[path = ...]` attribute)

All hits become edits in this PR (section 3).

### 1.3 CI green on all platforms

Before opening the PR, verify that the current master compiles and
passes tests on:

- Linux with `io_uring` feature (`cargo check --features io_uring`)
- Linux without `io_uring` feature (`cargo check --no-default-features`)
- macOS (`cargo check --target aarch64-apple-darwin`)
- Windows (`cargo check --target x86_64-pc-windows-msvc`)

These are the same gates the existing CI matrix enforces. No new CI
jobs needed.

## 2. Files to delete

The entire `crates/fast_io/src/io_uring_stub/` directory is removed:

| File | LoC | Original purpose |
|------|-----|-----------------|
| `mod.rs` | 113 | Module re-exports |
| `config.rs` | 75 | `StubIoUringBackend` (old), `is_io_uring_available()` |
| `shared_ring.rs` | 99 | Stub `SharedRing` struct + methods |
| `disk_batch.rs` | 88 | Stub `IoUringDiskBatch` + `Write` impl |
| `session_pool.rs` | 195 | Stub `SessionRingPool`, `RingLease`, `ThreadLocalRingPool` |
| `file_factory.rs` | 293 | Reader/writer factories, enum dispatchers |
| `file_reader.rs` | 73 | Stub `IoUringReader` |
| `file_writer.rs` | 89 | Stub `IoUringWriter` |
| `buffer_ring.rs` | 167 | Stub `BufferRing`, `BgidAllocator` |
| `registered_buffers.rs` | 122 | Stub `RegisteredBufferGroup`, `RegisteredBufferSlot` |
| `statx.rs` | 109 | Stub `StatxArgs`, free functions |
| `linkat.rs` | 79 | Stub `LinkAtArgs`, free functions |
| `renameat2.rs` | 61 | Stub `RenameAt2Args`, free functions |
| `cancel.rs` | 76 | Stub `CancelOutcome`, free functions |
| `send_zc.rs` | 84 | Stub `ZeroCopySender`, free functions |
| `linked_chain.rs` | 107 | Stub `LinkedChain`, `CqeResult` |
| `per_thread_ring.rs` | 36 | Stub `with_ring` free function |
| `socket_factory.rs` | 135 | Stub socket reader/writer factories (`#[cfg(unix)]`) |
| `socket_reader.rs` | 31 | Stub `IoUringSocketReader` |
| `socket_writer.rs` | 38 | Stub `IoUringSocketWriter` |
| `tests.rs` | 409 | 30+ stub-specific tests |
| **Total** | **2,479** | |

All 21 files are removed in a single commit. No file survives.

## 3. Module declaration and cfg-gate changes

### 3.1 `crates/fast_io/src/lib.rs`

**Before (current):**

```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod io_uring;
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
#[path = "io_uring_stub/mod.rs"]
pub mod io_uring;
```

**After:**

```rust
pub mod io_uring;
```

The dual cfg-gated declarations collapse to a single unconditional
`pub mod io_uring;`. Platform selection moves to `io_uring/mod.rs`
(which already has cfg-gated `mod backend_impl` / `mod backend_stub`
from IUS-8.b.1 and IUS-8.c.1). This is the key structural change -
the `io_uring` module is always compiled; the cfg gate is internal.

### 3.2 `crates/fast_io/src/io_uring/mod.rs`

Remove the doc-comment reference to `io_uring_stub.rs`:

**Before (line 29):**

```
//! module (`io_uring_stub.rs`) provides the same public API with
```

**After:**

```
//! module (`backend_stub.rs`) provides the same `IoUringBackend` trait
```

Confirm the cfg-gated backend declarations from IUS-8.c.1 are present:

```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod backend_impl;

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
mod backend_stub;
```

If the `io_uring/mod.rs` file previously conditionally compiled certain
submodules only on Linux (e.g., `mod shared_ring`, `mod statx`), these
gates must now be audited. Submodules containing Linux-only FFI remain
gated; submodules containing only cross-platform logic or types that are
re-exported through the trait become unconditional or are re-exported
from `io_uring_common.rs`.

### 3.3 Doc-comment updates

Six references to `io_uring_stub` exist outside the stub directory and
must be updated:

| File | Line(s) | Current text | Replacement |
|------|---------|-------------|-------------|
| `io_uring_common.rs` | 5 | `portable fallback ([crate::io_uring_stub])` | `portable fallback (backend_stub.rs)` |
| `io_uring_common.rs` | 25 | `crate::io_uring_stub (no-op)` | `backend_stub (no-op)` |
| `io_uring_common.rs` | 533 | `stub ([crate::io_uring_stub])` | `stub (backend_stub.rs)` |
| `io_uring/mod.rs` | 29 | `io_uring_stub.rs` | `backend_stub.rs` |
| `io_uring/buffer_ring/mod.rs` | 57 | `io_uring_stub.rs` | `backend_stub.rs` |
| `io_uring/renameat2.rs` | 64 | `[crate::io_uring_stub]` | `backend_stub` |

All replacements are doc-comment-only. No code logic changes.

### 3.4 `io_uring/mod.rs` re-exports

The current stub `mod.rs` re-exports ~50 symbols (types, functions,
constants). After IUS-8.b.2, callers import through the trait or through
`io_uring_common`. Verify that the `io_uring/mod.rs` re-exports cover
every symbol that external callers previously obtained from the
stub module path.

Key re-exports to verify still exist in `io_uring/mod.rs`:

- `is_io_uring_available` (free-function shim)
- `sqpoll_fell_back` (free-function shim)
- `IoUringOrStdReader`, `IoUringOrStdWriter` (factory types)
- `IoUringReaderFactory`, `IoUringWriterFactory` (factory types)
- `writer_from_file`, `reader_from_path` (free functions)
- `IoUringDiskBatch` (disk batch type)
- `SharedRing`, `SessionRingPool` (ring types)
- `StatxArgs`, `LinkAtArgs`, `RenameAt2Args` (argument structs)

On non-Linux, these are provided by `backend_stub.rs` or the
backward-compat shims described in IUS-8.c.1 section 7. The deletion PR
verifies that `cargo check` succeeds on all platforms after the stub
tree is removed, confirming no missing re-exports.

## 4. Cargo.toml changes

### 4.1 No stub-specific dependencies

The stub directory uses only `std` types. The `crates/fast_io/Cargo.toml`
has no dependencies that exist solely for the stub. All dependencies are
either:

- Platform-universal (`rayon`, `thiserror`, `tracing`, `logging`,
  `filetime`, `permutation`)
- Linux-gated behind `[target.'cfg(target_os = "linux")'.dependencies]`
  (`io-uring`, `landlock`)
- Unix-gated (`memmap2`, `libc`, `rustix`, `dashmap`)
- Windows-gated (`windows-sys`)

**No Cargo.toml changes are needed.** No dependencies are added or
removed by the stub deletion.

### 4.2 Feature flags

No feature flag references the stub directory. The `io_uring` feature
gates the `io-uring` crate dependency and the `backend_impl.rs` module.
The stub is selected by the inverse cfg. No feature flag changes needed.

## 5. Verification strategy

### 5.1 Compile-time verification (4-target matrix)

The following commands must pass. They are the same as the existing CI
matrix:

```sh
# 1. Linux with io_uring feature (real backend)
cargo check --features io_uring

# 2. Linux without io_uring feature (stub backend)
cargo check --no-default-features

# 3. macOS native (stub backend)
cargo check --target aarch64-apple-darwin

# 4. Windows cross-compile (stub backend)
cargo check --target x86_64-pc-windows-msvc
```

### 5.2 Test verification

```sh
# Linux: full nextest (CI only)
cargo nextest run --workspace --all-features

# Stub-specific tests: backend_smoke.rs stub arm
cargo nextest run -p fast_io --all-features -E 'test(stub_)'
```

The 30+ tests that lived in `io_uring_stub/tests.rs` are replaced by
the `stub_tests` module in `backend_smoke.rs` (added by IUS-8.c.1).
Verify that `backend_smoke.rs` covers every method category:

- Availability booleans return `false`
- Diagnostic strings are non-empty
- Construction methods return `Err(Unsupported)`
- `drain_completions` returns an empty iterator
- `allocate_bgid` returns `Err(Exhausted)`
- `deallocate_bgid` is a no-op (does not panic)
- `bgid_remaining` returns `0`

### 5.3 Grep verification

After deletion, confirm zero residual references:

```sh
# Must return zero hits
git grep -c 'io_uring_stub' -- '*.rs' '*.toml'

# Must return zero hits (path references in cfg or doc)
git grep -c '#\[path.*io_uring_stub' -- '*.rs'
```

### 5.4 Documentation build

```sh
cargo doc --workspace --no-deps
```

Verify no broken intra-doc links. The `docsrs` configuration in
`Cargo.toml` includes `-D rustdoc::broken_intra_doc_links`, so the
doc build will fail on any dangling `[crate::io_uring_stub]` link.

## 6. Expected size reduction

### 6.1 Source code

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Files in `io_uring_stub/` | 21 | 0 | -21 |
| LoC in `io_uring_stub/` | 2,479 | 0 | -2,479 |
| LoC in `backend_stub.rs` | 0 | ~200 | +200 |
| LoC in shim/compat code | 0 | ~80 | +80 |
| **Net LoC delta** | | | **~-2,200** |

The `backend_stub.rs` (~200 LoC) and backward-compat shims (~80 LoC)
already exist from IUS-8.c.1. They are not added by this PR - they are
pre-existing. The net delta of this PR is purely deletive.

### 6.2 On-disk size

The `io_uring_stub/` directory is 120 KB on disk. After deletion, the
`backend_stub.rs` file (~200 lines) is approximately 8 KB. Net savings:
~112 KB of source.

### 6.3 Compile-time impact

The stub tree contains 21 compilation units that the compiler must parse
and type-check on every non-Linux build. Replacing them with a single
`backend_stub.rs` file reduces the parsing and name-resolution overhead.
Expected compile-time improvement on macOS/Windows incremental builds:
~0.5-1.0 seconds (minor, dominated by the rest of the workspace). The
benefit is more pronounced on clean builds and in CI where every crate
is compiled from scratch.

### 6.4 Maintenance cost reduction

- **Zero per-feature mirror edits.** Previously, every new io_uring
  entry point required a matching stub function. Now, adding a method to
  `IoUringBackend` produces a compile error in `backend_stub.rs` that
  requires adding exactly one 1-3 line return statement.
- **Zero signature drift risk.** The trait definition is the single
  source of truth. Both `backend_impl.rs` (Linux) and `backend_stub.rs`
  (non-Linux) implement the same trait signatures, enforced at compile
  time.
- **Simpler code review.** Feature PRs touching io_uring no longer
  include a mirrored stub diff. Reviewers see the Linux impl and the
  trait method only.

## 7. Migration sequence

### 7.1 Single-commit approach

The deletion is a single atomic commit. The rationale: all callers are
already migrated (IUS-8.b.2), the trait stub is already in place
(IUS-8.c.1), and the only changes are file deletion + doc-comment
updates + `lib.rs` cfg simplification. Splitting into multiple commits
would create intermediate states where the stub tree exists alongside
the new stub but is partially unreferenced - confusing for bisection.

### 7.2 Ordered steps within the commit

1. **Update `lib.rs`** - collapse the dual cfg-gated `pub mod io_uring`
   to a single unconditional declaration. Remove the `#[path = ...]`
   attribute.

2. **Update doc comments** - replace all 6 references to
   `io_uring_stub` (section 3.3).

3. **Delete `io_uring_stub/`** - `git rm -r crates/fast_io/src/io_uring_stub/`.

4. **Run 4-target compile check** - verify all platforms compile.

5. **Run stub tests** - verify `backend_smoke.rs` stub tests pass.

6. **Run grep verification** - confirm zero residual references.

## 8. Rollback plan

### 8.1 If deletion breaks downstream

The deleted files are pure no-op stubs. No runtime behavior changes.
If a downstream caller was missed by the IUS-8.b.2 audit (still imports
from `crate::io_uring_stub::*`), the build error is immediate and
specific:

```
error[E0433]: failed to resolve: could not find `io_uring_stub` in `crate`
```

**Fix:** add the missing re-export or free-function shim in
`io_uring/mod.rs`. This is a one-line additive change, not a rollback.

### 8.2 If the PR must be reverted

```sh
git revert <commit-sha>
```

The revert restores the 21 stub files and the `#[path = ...]` alias in
`lib.rs`. Because the trait stub (`backend_stub.rs`) coexists with the
old stub tree (both compiled via cfg), the revert compiles on all
platforms without further changes.

### 8.3 Risk assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Missed caller still imports from `io_uring_stub` | Low (IUS-8.b.2 audit is exhaustive) | Low (compile error, 1-line fix) | Pre-deletion grep (section 1.2) |
| Missing re-export on non-Linux | Low | Low (compile error on macOS/Windows CI) | 4-target compile matrix (section 5.1) |
| Broken doc links | Low | None (warning, not error in production) | `cargo doc` with `-D broken_intra_doc_links` (section 5.4) |
| Behavioral regression on non-Linux | None | None | Stub behavior is unchanged - `backend_stub.rs` returns the same values as the deleted stub files |

Overall risk: **minimal.** The change is purely deletive with no
behavioral delta. The trait enforces API parity at compile time. CI
catches any missed references on every target platform.

## 9. Relationship to IUS-9

IUS-9 (provisional) is the follow-up that deprecates and eventually
removes the free-function shims (`is_io_uring_available()`,
`writer_from_file()`, etc.) that IUS-8.c.1 added for backward
compatibility. IUS-8.c.2 does not touch these shims. They remain as
thin wrappers calling through the `PlatformIoUringBackend` type alias
or returning constant stub values directly.

The IUS-9 timeline is decoupled from IUS-8.c.2. The shims are low-cost
(~80 LoC total) and carry no maintenance burden because they forward to
the trait. They can live indefinitely until all callers are migrated.

## 10. Success criteria

- [ ] `crates/fast_io/src/io_uring_stub/` directory does not exist
- [ ] `git grep 'io_uring_stub' -- '*.rs' '*.toml'` returns zero hits
- [ ] `lib.rs` has a single unconditional `pub mod io_uring;`
- [ ] CI passes on all 4 targets: Linux+feature, Linux-no-feature,
      macOS, Windows
- [ ] `cargo doc --workspace --no-deps` produces no broken-link warnings
- [ ] `backend_smoke.rs` stub tests pass on macOS and Windows CI runners
- [ ] Net LoC delta is negative (~2,200 lines removed)

---

**Summary.** This PR deletes 21 files and 2,479 lines of mechanical
stub duplication. The `IoUringBackend` trait (IUS-8.a) and its non-Linux
impl (`backend_stub.rs`, IUS-8.c.1) structurally guarantee API parity
across platforms at compile time, making the file-mirroring approach
obsolete. The deletion is purely subtractive - no new code, no
behavioral changes, no new dependencies. Risk is minimal because the
stub tree is already unreachable from external callers after IUS-8.b.2.
