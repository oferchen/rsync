# shared_ring removal plan (IUR-6.b)

Tracking: IUR-6.b. Predecessors:

- `docs/audit/shared-ring-callers-post-migration.md` - IUR-6.a inventory
  that identified the removal surface.
- `docs/design/iur-3f-shared-rings-decision.md` - IUR-3.f decision
  record: probes and disk-commit ring stay shared.
- `docs/design/iur-3g-zerocopysender-migration-deferred.md` - IUR-3.g
  deferral of `ZeroCopySender` per-thread migration.
- PR #5251 (IUR-6.c) - CI lint guard that greps factory modules for
  `Arc<Mutex` patterns to prevent shared-ring regression.

## 1. Current shared_ring callers (IUR-6.a summary)

IUR-6.a classified every remaining non-per-thread ring caller into two
buckets:

### 1.1 CANDIDATE-FOR-REMOVAL (dormant, zero production callers)

| Symbol | Location | Purpose |
|--------|----------|---------|
| `SharedRing` type + methods | `crates/fast_io/src/io_uring/shared_ring.rs` | Co-locates reader+writer fd on one ring; replaced by per-thread rings (IUR-3) |
| `SharedRingConfig` | `crates/fast_io/src/io_uring_common.rs:193` | Plain-data config for `SharedRing` |
| `SharedCompletion` | `crates/fast_io/src/io_uring_common.rs:269` | CQE demux enum for `SharedRing::reap` |
| `SessionRingPool` / `RingLease` | `crates/fast_io/src/io_uring/session_pool.rs:145-220` | Bounded mutex-fleet pool; no production caller |
| Stub `SharedRing` | `crates/fast_io/src/io_uring_stub/shared_ring.rs` | Non-Linux API mirror; follows Linux type |

### 1.2 KEEP-SHARED (intentional, ratified by decision records)

| Symbol | Location | Decision |
|--------|----------|----------|
| One-shot probes (`linkat`, `renameat2`, `statx`) | `linkat.rs:113`, `renameat2.rs:76`, `statx.rs:138` | IUR-3.f section 2; throwaway ring, result cached in `OnceLock<bool>` |
| `IoUringDiskBatch` | `disk_batch.rs:45` | IUR-3.f section 3; single-owner, disk-commit thread |
| `ZeroCopySender` (`Arc<Mutex<RawIoUring>>`) | `send_zc.rs:284` | IUR-3.g; deferred behind IUS-8, opt-in feature |
| `IoUringSocketReader` | `socket_reader.rs:32` | IUR-2 section 1.1; one reader per session |
| `ThreadLocalRingPool` / `ThreadLocalRingLease` | `session_pool.rs:332-422` | Per-thread side of pool; forward-looking, not removal surface |

### 1.3 KEEP (reusable data types, independent of SharedRing)

| Symbol | Location | Reason |
|--------|----------|--------|
| `OpTag` | `io_uring_common.rs:222-265` | 8-bit tag + 56-bit op_id demux scheme; used by `cancel.rs`; reusable by any multi-op CQ consumer |

## 2. Legitimate vs removable uses

### 2.1 Removable: the `SharedRing` abstraction

`SharedRing` is dormant infrastructure. The IUR-1 audit (section 2.2)
confirmed it has zero production callers; the IUR-3 migration did not
introduce any. It was the proposed topology for co-locating reader and
writer fds on one ring, but `per_thread_ring::with_ring` now serves that
role without the serialization overhead.

Files to delete or gut:

1. `crates/fast_io/src/io_uring/shared_ring.rs` - the entire module
2. `crates/fast_io/src/io_uring_stub/shared_ring.rs` - the stub mirror
3. `crates/fast_io/tests/io_uring_shared_ring.rs` - integration tests
4. `crates/fast_io/benches/iouring_per_file_vs_shared.rs` - bench (or
   retain as a per-thread-vs-standard comparison; see section 4.3)

### 2.2 Removable: `SessionRingPool` (the mutex-fleet pool)

`SessionRingPool` has no production caller. Its tests
(`session_pool.rs:518-609`) and the `linked_chain.rs` examples are the
only consumers. `ThreadLocalRingPool` (same file, lines 332-422) IS the
production alternative and stays.

### 2.3 Removable: `SharedRingConfig` and `SharedCompletion`

Both are data-only types consumed exclusively by `SharedRing`. They live
in `io_uring_common.rs` and the stub re-export chain. Once `SharedRing`
is removed, they have no consumer.

### 2.4 Preserved: `OpTag`

`OpTag` is the 8-bit-tag + 56-bit-op-id encoding scheme for `user_data`
in SQEs/CQEs. Although it is re-exported through `shared_ring.rs:85`
today, it lives in `io_uring_common.rs` and is referenced by
`cancel.rs:388-389`. It remains useful for any multi-op ring consumer
(including `disk_batch` and future per-thread batched submitters). The
re-export path changes but the type survives.

### 2.5 Not touched: one-shot probes

The one-shot probes (`linkat_supported`, `renameat2_supported`,
`statx_supported`) each build a throwaway 2-entry ring, probe one
opcode, cache the result in a process-wide `OnceLock<bool>`, and drop
the ring. This is the correct shape: the ring exists only for the
duration of the probe. No change is needed. They are not part of the
`shared_ring.rs` module and do not depend on the `SharedRing` type.

The `send_zc::probe_send_zc` function (`send_zc.rs:97-106`) follows the
same pattern: a throwaway `IoUring::new(4)` scoped to the function body.
No dependency on `SharedRing`.

### 2.6 Not touched: disk-commit ring

`IoUringDiskBatch` owns its ring as a plain `RawIoUring` field. It does
not use `SharedRing`. Its `!Send + !Sync` bounds enforce single-thread
ownership. It lives in `disk_batch.rs` and is unaffected by the removal.

### 2.7 Not touched: `ZeroCopySender`

`ZeroCopySender` uses `Arc<Mutex<RawIoUring>>` (not `SharedRing`). Its
`from_shared_ring` method name is misleading - it accepts an
`Arc<Mutex<RawIoUring>>`, not a `SharedRing`. The type is deferred behind
IUS-8 (IUR-3.g) and is default-off. No dependency on `SharedRing`.

## 3. Migration plan for each caller

### 3.1 `SharedRing` type and module

**Action:** Delete entirely.

No migration needed because there are zero production callers. The type
was infrastructure-ahead-of-demand that IUR-3 superseded with the
per-thread ring primitive.

### 3.2 `SharedRingConfig`

**Action:** Delete from `io_uring_common.rs`.

No caller outside `SharedRing` uses it. Remove the struct definition
and the re-exports in:
- `crates/fast_io/src/io_uring/mod.rs:191`
- `crates/fast_io/src/lib.rs:317`
- `crates/fast_io/src/io_uring_stub/shared_ring.rs:7`
- `crates/fast_io/src/io_uring_stub/mod.rs:66`

### 3.3 `SharedCompletion`

**Action:** Delete from `io_uring_common.rs`.

Same rationale as `SharedRingConfig`. Remove the enum and all re-exports.

### 3.4 `OpTag`

**Action:** Preserve in `io_uring_common.rs`. Update re-export path.

Currently re-exported through `shared_ring.rs:85` then
`io_uring/mod.rs:191`. After `shared_ring.rs` is deleted, re-export
`OpTag` directly from `io_uring/mod.rs` via `io_uring_common`. The stub
module (`io_uring_stub/mod.rs`) already imports from `io_uring_common`
and continues to work.

### 3.5 `SessionRingPool` / `RingLease`

**Action:** Delete the `SessionRingPool` and `RingLease` types from
`session_pool.rs`. Preserve `ThreadLocalRingPool` and
`ThreadLocalRingLease` (they are the forward-looking primitives).

Update `io_uring/mod.rs` re-exports to remove `RingLease`,
`SessionPoolConfig` (if only used by `SessionRingPool`),
`SessionRingPool`. Check whether `SessionPoolConfig` is shared with
`ThreadLocalRingPool` - if so, keep it.

Note: `SessionPoolConfig` IS used by `ThreadLocalRingPool::new` (takes
it as parameter). Therefore `SessionPoolConfig` stays; only
`SessionRingPool` and `RingLease` are removed.

### 3.6 Stub module `io_uring_stub/shared_ring.rs`

**Action:** Delete entirely. Remove `pub mod shared_ring;` from
`io_uring_stub/mod.rs:48` and the corresponding re-export at
`io_uring_stub/mod.rs:100`.

### 3.7 Test file `tests/io_uring_shared_ring.rs`

**Action:** Delete. The `OpTag` round-trip tests can be relocated into
`io_uring_common.rs` as `#[cfg(test)]` unit tests (they are pure
arithmetic, no ring needed).

### 3.8 Bench file `benches/iouring_per_file_vs_shared.rs`

**Action:** Delete or repurpose. The bench compares per-file ring
creation vs ring reuse (the `IoUringDiskBatch` pattern). If the bench
provides ongoing value for disk-batch tuning, rename it to
`iouring_per_file_vs_batched.rs` and rewire the "shared" arm to use
`IoUringDiskBatch` directly. If not, delete it.

Decision: **delete** - the bench was created to inform the IUR-2 decision
(per-thread vs shared). That decision is made. The `IUR-4a` 100K
submission stress harness is the successor bench.

### 3.9 References in `io_uring/mod.rs` comments and re-exports

**Action:** Remove `pub mod shared_ring;` (line 124), remove the
`pub use shared_ring::{...}` (line 191), update the re-export of `OpTag`
to come directly from `io_uring_common`. Audit in-module doc comments
that reference `shared_ring` (e.g., `session_pool.rs:38`,
`per_thread_ring.rs:14`, `linkat.rs:102`, `renameat2.rs:8`,
`statx.rs:130`) and rewrite to reference "the legacy shared-ring pattern
(removed)" or "the IUR-3 per-thread ring" as appropriate.

### 3.10 References in `lib.rs` re-exports

**Action:** Remove `SharedRing`, `SharedRingConfig`, `SharedCompletion`
from the `pub use` statement at `lib.rs:317-318`. Keep `OpTag`.

## 4. Removal order (dependency graph)

The removal is structured as a linear sequence where each step compiles
independently. This allows incremental review and per-step CI
verification.

```
Step 1: Relocate OpTag tests
  └── Step 2: Delete shared_ring.rs (Linux)
        └── Step 3: Delete shared_ring.rs (stub)
              └── Step 4: Delete SharedRingConfig + SharedCompletion
                    └── Step 5: Delete SessionRingPool + RingLease
                          └── Step 6: Delete test + bench files
                                └── Step 7: Update re-exports + docs
```

### Step 1: Relocate `OpTag` round-trip tests

Move the `OpTag` encoding tests from
`tests/io_uring_shared_ring.rs:16-32` into
`crates/fast_io/src/io_uring_common.rs` as a `#[cfg(test)] mod tests`
block. Verify they pass standalone.

**Files touched:** `io_uring_common.rs` (add tests), no deletions yet.

### Step 2: Delete `shared_ring.rs` (Linux module)

Remove `crates/fast_io/src/io_uring/shared_ring.rs`. Remove the
`pub mod shared_ring;` declaration from `io_uring/mod.rs:124`. Update
the `pub use` at `io_uring/mod.rs:191` to export `OpTag` directly from
`super::io_uring_common` instead of from `shared_ring`. Remove
`SharedCompletion`, `SharedRing`, `SharedRingConfig` from that re-export
line.

**Files touched:** delete `shared_ring.rs`, edit `io_uring/mod.rs`.

### Step 3: Delete stub `shared_ring.rs`

Remove `crates/fast_io/src/io_uring_stub/shared_ring.rs`. Remove
`pub mod shared_ring;` from `io_uring_stub/mod.rs:48`. Remove
`pub use shared_ring::SharedRing;` from `io_uring_stub/mod.rs:100`.
Remove `SharedCompletion, SharedRingConfig` from the `io_uring_common`
re-export at `io_uring_stub/mod.rs:66`.

**Files touched:** delete `io_uring_stub/shared_ring.rs`, edit
`io_uring_stub/mod.rs`.

### Step 4: Delete `SharedRingConfig` and `SharedCompletion`

Remove both type definitions from `io_uring_common.rs`. Remove
their mention from the module-level doc comment at `io_uring_common.rs:12`.

**Files touched:** edit `io_uring_common.rs`.

### Step 5: Delete `SessionRingPool` and `RingLease`

Remove the `SessionRingPool` struct, its `impl` blocks, `RingLease`,
and the `build_ring` helper from `session_pool.rs`. Preserve
`ThreadLocalRingPool`, `ThreadLocalRingLease`, `SessionPoolConfig`,
the `THREAD_RINGS` thread-local, and the `NEXT_POOL_ID` atomic.
Remove the `SessionRingPool` / `RingLease` tests (lines 467-609).

Update `io_uring/mod.rs` and `lib.rs` re-exports to remove
`RingLease` and `SessionRingPool`.

**Files touched:** edit `session_pool.rs`, `io_uring/mod.rs`, `lib.rs`.

### Step 6: Delete test and bench files

Remove `crates/fast_io/tests/io_uring_shared_ring.rs`. Remove the
`[[bench]]` entry for `iouring_per_file_vs_shared` from
`crates/fast_io/Cargo.toml`. Remove
`crates/fast_io/benches/iouring_per_file_vs_shared.rs`.

Check for references in `tests/io_uring_mmap_pressure.rs` - if it
imports `SharedRing`, audit and update (likely just remove the
`SharedRing` test paths, keeping the mmap-pressure tests that use the
per-thread ring).

**Files touched:** delete test/bench files, edit `Cargo.toml`.

### Step 7: Update documentation and comments

Audit all Markdown and Rust comments that reference `shared_ring`,
`SharedRing`, or the co-located reader/writer topology. Update to
reference the per-thread ring or mark as historical. Key files:

- `docs/design/io-uring-shared-ring-audit.md` - add deprecation note
- `docs/design/iur-3f-shared-rings-decision.md` - add note that the
  removal is complete
- `crates/fast_io/src/io_uring/session_pool.rs:38` - rewrite comment
- `crates/fast_io/src/io_uring/per_thread_ring.rs:14` - rewrite comment
- `crates/fast_io/src/io_uring/linkat.rs:102` - rewrite comment
- `crates/fast_io/src/io_uring/renameat2.rs:8` - rewrite comment
- `crates/fast_io/src/io_uring/statx.rs:130` - rewrite comment

**Files touched:** multiple doc/comment edits.

## 5. Probe operation fallback

### 5.1 Current probe pattern (unchanged)

Each one-shot probe (`linkat_supported`, `renameat2_supported`,
`statx_supported`, `send_zc::is_supported`) already follows the
correct pattern:

```rust
static SUPPORTED: OnceLock<bool> = OnceLock::new();

fn probe_opcode() -> bool {
    let Ok(ring) = io_uring::IoUring::new(2) else {
        return false;
    };
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_err() {
        return false;
    }
    probe.is_supported(OPCODE)
}
```

The ring is scoped to the function body, built and dropped within a
single call. It does not use `SharedRing`, `SharedRingConfig`, or any
removed type. No change is required.

### 5.2 Why not a module-level helper

A tempting refactor would consolidate the four probes into a single
`probe_opcode(code: u8) -> bool` helper. This was considered and
rejected:

- Each probe site has its own `OnceLock<bool>` static, which is correct:
  the opcodes are independent and should be cached independently.
- A shared helper would need to either accept a static reference
  (lifetime gymnastics) or return a bool that each caller caches.
- The current pattern is 8 lines per probe. A helper saves 5 lines per
  site at the cost of an indirection layer that obscures the direct
  kernel interaction.
- The probes are stable, never change, and each is called exactly once
  per process. Code sharing adds complexity without reducing bugs.

Verdict: leave probes as inline per-site functions. No helper module.

### 5.3 The `SharedRing::probe_poll_add` internal helper

The `probe_poll_add` function inside `shared_ring.rs:368-377` is private
to `SharedRing` and is only called from `SharedRing::new_inner`. When
`SharedRing` is deleted, `probe_poll_add` goes with it. No migration
needed - the per-thread ring primitive does not need a POLL_ADD probe
because its callers do not use POLL_ADD (they use READ/WRITE opcodes
directly).

## 6. Risk assessment and rollback strategy

### 6.1 Risk: zero

The removal surface has **zero production callers** (confirmed by IUR-1
section 2.2, IUR-6.a section "Removal readiness conclusion" item 1). No
code in `engine`, `transfer`, `daemon`, `core`, or any crate outside
`fast_io` constructs or borrows a `SharedRing`. The types being removed
are dead code that happens to compile.

### 6.2 Compile-time verification

Each step in the removal order (section 4) is designed to leave the
codebase in a compiling state. CI verification after each step
(`cargo clippy --workspace --all-targets --all-features`) catches any
missed dependency. The removal is additive-negative (only deletions),
so new compilation errors indicate a missed caller - the fix is to
update that caller, not to revert.

### 6.3 IUR-6.c lint guard

PR #5251 merged a CI lint guard that greps factory modules for
`Arc<Mutex` patterns. This guard prevents re-introduction of the
shared-ring anti-pattern after removal. It runs on every PR and will
fail if any new code adds `Arc<Mutex<IoUring>>` to the factory modules.

### 6.4 Rollback strategy

If an unforeseen downstream consumer is discovered after removal:

1. **Immediate:** `git revert` the removal commit(s). The types are
   self-contained and have no cross-module state, so revert is clean.
2. **Root-cause:** Identify the consumer that was missed by IUR-6.a.
   Determine whether it should migrate to per-thread (IUR-3.a pattern)
   or stay on its own ring (IUR-3.f pattern).
3. **Re-land:** Re-land the removal with an exception for the discovered
   consumer, following the IUR-3.f/3.g decision template.

Risk of needing rollback: effectively zero, because the IUR-6.a
inventory was exhaustive and confirmed by grep.

### 6.5 Feature-flag coupling

The removal does not affect any cargo feature flag. `SharedRing` is not
gated behind a feature; it compiles unconditionally on Linux and as a
stub on other platforms. Removal simplifies the `io_uring` / `io_uring_stub`
module split by eliminating one of the modules that must be mirrored.

### 6.6 Timing

The removal is gated on the IUR-5 benchmark decision per
`iur-2-per-thread-rings.md` section 7. The benchmark
(`iouring_per_file_vs_shared.rs`) exists to confirm the per-thread ring
is not a regression vs the shared topology. Once IUR-5 signs off (or is
bypassed because the bench target has zero production callers and the
comparison is academic), IUR-6.b can proceed.

## 7. Summary

| What | Action | Risk |
|------|--------|------|
| `SharedRing` module (Linux) | Delete | None (zero callers) |
| `SharedRing` stub (non-Linux) | Delete | None (follows Linux) |
| `SharedRingConfig` | Delete | None |
| `SharedCompletion` | Delete | None |
| `OpTag` | Keep, update re-export path | None |
| `SessionRingPool` / `RingLease` | Delete | None (zero callers) |
| `ThreadLocalRingPool` | Keep | N/A |
| `IoUringDiskBatch` | Keep (IUR-3.f) | N/A |
| One-shot probes | Keep (IUR-3.f) | N/A |
| `ZeroCopySender` | Keep (IUR-3.g) | N/A |
| Tests/benches for removed types | Delete | None |
| Doc/comment references | Update | None |

Total files deleted: 4 (shared_ring.rs, stub shared_ring.rs, test, bench).
Total files edited: ~12 (mod.rs, lib.rs, io_uring_common.rs, session_pool.rs,
Cargo.toml, plus doc/comment updates in 6-7 files).

The removal is mechanical: delete dead code, update re-exports, fix
comments. No behavioural change to any live path.
