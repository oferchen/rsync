# BR-3j.a: DashMap vs Sharded Map for `ParallelDeltaApplier.files` (2026-05-20)

Read-only design audit for issue #2503. Picks the replacement strategy for the
outer `Mutex<HashMap<FileNdx, Arc<Mutex<FileSlot>>>>` map at
`crates/engine/src/concurrent_delta/parallel_apply.rs:252` that today serialises
every per-chunk slot lookup behind a single global mutex. Follow-up tasks
BR-3j.b (bench), BR-3j.c (implement), and BR-3j.d (wire-in) execute against
the pick recorded here.

## Background

`ParallelDeltaApplier` fans the CPU-bound verify step across the rayon pool
and then serialises the destination write under a per-file `Mutex<FileSlot>`.
The per-file mutex is the only lock that bytes need to clear; the outer mutex
exists solely to look the inner `Arc` up. Every chunk submitted through
`apply_chunk_parallel` (`parallel_apply.rs:356-368`) and every entry in the
batch path (`parallel_apply.rs:380-405`) calls `slot_for`
(`parallel_apply.rs:467-476`), which takes that outer lock for the duration of
the `HashMap::get` plus the `Arc::clone`. Under N rayon workers all touching
distinct files, the single outer lock still serialises every lookup, defeating
the per-file fanout the rest of the applier is built around.

## Access pattern summary

The map sees three call sites today, all inside `parallel_apply.rs`:

- `register_file` (`parallel_apply.rs:318-341`): one insert per file, called
  once before any chunk arrives. Cold path.
- `slot_for` (`parallel_apply.rs:467-476`): one read-and-clone per chunk,
  called from `apply_chunk_parallel`, `apply_batch_parallel`, and
  `bytes_written`. Hot path; this is the contention source.
- `finish_file` (`parallel_apply.rs:434-465`): one remove per file at
  shutdown, called once after the last chunk has drained. Cold path.

The `Debug` impl (`parallel_apply.rs:262`) reads `.len()` for diagnostics.
That is the only iteration site; no production caller iterates the map. A
codebase search (`grep -rn "ParallelDeltaApplier" crates --include="*.rs"`)
shows callers only via `register_file`/`apply_chunk_parallel`/
`apply_batch_parallel`/`finish_file`/`bytes_written` plus tests and benches
under `crates/engine/benches/parallel_receive_delta_perf.rs` and
`crates/engine/tests/arc_drain_panic_recovery.rs`.

The value type is `Arc<Mutex<FileSlot>>` (clone is cheap, 2 atomic ops). The
key type is `FileNdx` (`crates/engine/src/concurrent_delta/types.rs:23-25`),
a `#[repr(transparent)] u32` with `Hash + Eq + Copy`.

## Options

1. **DashMap** - `dashmap::DashMap<FileNdx, Arc<Mutex<FileSlot>>>`. Already a
   workspace dependency, pinned to `dashmap = "6.1"` in `Cargo.toml:75-76`,
   currently runtime-gated behind the daemon's `concurrent-sessions` feature
   (`crates/daemon/Cargo.toml:27,51`) and used in
   `crates/daemon/src/daemon/connection_pool/pool.rs:11` and
   `crates/daemon/src/daemon/session_registry.rs:13`. In the engine crate it
   is presently a **dev-dependency only**
   (`crates/engine/Cargo.toml:176`), pulled in by the contention bench at
   `crates/engine/benches/delete_plan_map_contention.rs`. Internally sharded
   (`shard_amount` defaults to `4 * num_cpus().next_power_of_two()`), uses
   `RwLock` per shard via `lock_api` + `parking_lot_core`. Access returns
   `Ref<K, V>` / `RefMut<K, V>` guards that deref to the value.

2. **Sharded hand-rolled** - `Box<[Mutex<HashMap<FileNdx, Arc<Mutex<FileSlot>>>>]>`
   sized to `next_power_of_two(num_cpus())`. Shard key = `(ndx.get() as usize)
   & (N - 1)` (the `FileNdx` `u32` is already a dense small integer; modulo
   reduces to a mask once `N` is a power of two and no hashing is needed).
   Standard library only, zero new dependencies in the engine crate. Same
   triplet of `insert` / `remove` / `get_clone` API as today, just per-shard.

The bench at `crates/engine/benches/delete_plan_map_contention.rs` already
exists for the sibling `DeletePlanMap` choice and lays out the same three-way
contest (`mutex_hashmap`, `dashmap`, `sharded_mutex_hashmap`). BR-3j.b should
clone that harness shape for the applier's access mix (mostly `get`, rare
`insert`/`remove`).

## Side-by-side comparison

| Criterion | DashMap 6.1 | Sharded `Mutex<HashMap>` |
|---|---|---|
| Read (get + Arc clone) under contention | One `RwLock` read on the target shard via `parking_lot_core`. Multiple readers on the same shard proceed in parallel. Different shards never collide. | One `Mutex` lock on the target shard. Two readers on the same shard serialise even though both only need to clone an `Arc`. Different shards never collide. |
| Write (insert / remove) under contention | One `RwLock` write on the target shard; blocks readers on that shard for the duration of the hash-map op (microseconds). | One `Mutex` lock on the target shard; same scope. Equivalent. |
| Memory overhead (empty) | `4 * num_cpus().next_power_of_two()` shards, each an empty `HashMap` plus a `RwLock`. On an 8-core box: 32 shards. Roughly 32 * (~56 B `HashMap` header + lock state) ~= 2-3 KB before any insert. | `next_power_of_two(num_cpus())` shards, each an empty `HashMap` plus a `Mutex`. On an 8-core box: 8 shards. Roughly 8 * (~56 B header + ~48 B `Mutex`) ~= 0.8-1 KB. Smaller, but the difference is irrelevant against per-file `FileSlot` writers. |
| Memory overhead (steady state, 10k files) | Hash buckets distributed across 32 shards; total entry overhead matches a single `HashMap` of the same size (DashMap shards keep their own bucket arrays). | Same total bucket count split across 8 shards. Practically identical to DashMap for any non-trivial file count. |
| API ergonomics for `slot_for` | `self.files.get(&ndx).map(|r| Arc::clone(r.value()))`. The `Ref` guard holds a shard read lock for the duration of the closure - cheap and short. | `let s = self.shard_for(ndx); self.shards[s].lock()...get(&ndx).map(Arc::clone)`. One extra index op and one extra `lock().unwrap()` map, both cheap. |
| API ergonomics for `register_file` | `match self.files.entry(ndx) { Entry::Occupied(_) => Err(...), Entry::Vacant(v) => { v.insert(Arc::new(...)); Ok(()) } }`. Atomic insert-if-missing in one call - no TOCTOU window between `contains_key` and `insert`. | `let mut shard = self.shards[s].lock()...; if shard.contains_key(&ndx) { Err(...) } else { shard.insert(ndx, ...); Ok(()) }`. Also atomic because the shard lock spans both ops; mechanically equivalent but two map calls instead of one. |
| API ergonomics for `finish_file` | `self.files.remove(&ndx).ok_or(...)`. Returns `(K, V)`, takes the value back as `Arc<Mutex<FileSlot>>`. | `let mut shard = self.shards[s].lock()...; shard.remove(&ndx).ok_or(...)`. Identical shape after the shard lookup. |
| Iteration (`Debug::fmt` `.len()`) | `self.files.len()` is O(shards): sums each shard's length. Acceptable for `Debug`. Full iteration locks one shard at a time and yields `RefMulti` guards. | `self.shards.iter().map(|s| s.lock().unwrap().len()).sum()`. Same big-O shape. Full iteration must lock each shard in turn, identical semantics. |
| Mutex-poisoning surface | DashMap's `parking_lot_core` locks are panic-safe and **do not poison**; the lock is released on panic. One fewer `.map_err(...)` rung in every call site. | Two layers of `std::sync::Mutex` (outer-shard and per-file `FileSlot`) each carry the existing `"parallel applier file map poisoned"` / `"parallel applier file slot poisoned"` mapping. Behaviour is preserved but the error budget stays exactly as it is today. |
| Lookup-time hashing | DashMap hashes `FileNdx` with the standard library's default hasher (or a `BuildHasher` passed at construction). One hash + one shard index. | `FileNdx & (N - 1)`. No hashing for shard selection - dense u32 keys are uniform enough; the inner `HashMap` still hashes for bucket selection. Slightly cheaper shard pick; same inner cost. |
| Dependency / build cost | **No new dependency**, but promotes `dashmap` from dev-dep to runtime dep in `crates/engine/Cargo.toml`. Pulls `parking_lot_core`, `lock_api`, `crossbeam-utils`, and `hashbrown 0.14.5` (already in the workspace tree per `Cargo.lock`). Marginal compile-time addition. | Zero new dependencies. Pure `std` + the existing `std::sync::Mutex` / `std::collections::HashMap` machinery. |
| Code surface in `parallel_apply.rs` | `slot_for`/`register_file`/`finish_file` shrink by ~3 lines each (no outer lock-and-unwrap, no `files.insert`+`contains_key` split). Roughly 8 LoC saved. | `slot_for`/`register_file`/`finish_file` keep the same shape; a new private `shard_for(ndx) -> usize` helper plus a 4-line constructor that builds `next_power_of_two(num_cpus().get())` shards. Roughly 15 LoC added. |
| MSRV | `dashmap 6.1.0` declares `rust-version = 1.65` (verified via `cargo info dashmap`). Comfortably below the workspace MSRV of 1.88 pinned in `rust-toolchain.toml:2`. **No blocker.** | n/a - std only. |
| RUSTSEC / audit history | DashMap 6.x is the current stable line; the workspace already runs it in the daemon hot path under `concurrent-sessions`. No open advisories against 6.1.0 in the lockfile. | n/a. |
| Tunability | `DashMap::with_shard_amount(n)` allows the applier to dial shards independently of `num_cpus` if BR-3j.b shows the default 4xCPU is wasteful for our access mix. | Shard count is a one-line constant. Equally tunable. |
| Failure-mode visibility | Errors flow through the existing `io::Error::other("parallel applier file ... poisoned")` mappings only when *inner* `FileSlot` mutexes poison; the outer DashMap cannot poison. | Both outer-shard and inner mutexes can poison; the typed `ParallelApplyError::SlotPoisoned` (`parallel_apply.rs:84-90`) already covers the inner case. The outer-shard message would need its own mapping or a generic "parallel applier shard poisoned for ndx=N" string. |

## The pick

**DashMap.**

The applier's workload is overwhelmingly read-and-clone-Arc (one per chunk,
N-way concurrent across rayon workers), with insert/remove only at
file-registration and finish. DashMap's per-shard `RwLock` lets two workers
on the same shard read in parallel where a hand-rolled `Mutex<HashMap>` shard
still serialises them; with the inner `FileSlot` mutex already providing the
write-side exclusion, the outer map only needs to be cheap to traverse. The
dependency cost is nominal - `dashmap` is already a workspace dep used in the
daemon, MSRV 1.65 is well below our 1.88, the engine crate already pulls it
as a dev-dep for the sibling bench, and the `Entry` API removes a TOCTOU
window in `register_file` that the hand-rolled shard would have to re-derive
with a held shard lock.

## API sketch (no code edits)

### Today (`parallel_apply.rs:248-476`, abridged)

```rust
pub struct ParallelDeltaApplier {
    files: Mutex<HashMap<FileNdx, Arc<Mutex<FileSlot>>>>,
    per_file_reorder_capacity: usize,
    concurrency: usize,
}

fn slot_for(&self, ndx: FileNdx) -> io::Result<Arc<Mutex<FileSlot>>> {
    let files = self.files.lock()
        .map_err(|_| io::Error::other("parallel applier file map poisoned"))?;
    files.get(&ndx).map(Arc::clone)
        .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))
}

pub fn register_file(&self, ndx: impl Into<FileNdx>, writer: Box<dyn Write + Send>)
    -> io::Result<()>
{
    let ndx = ndx.into();
    let mut files = self.files.lock()
        .map_err(|_| io::Error::other("parallel applier file map poisoned"))?;
    if files.contains_key(&ndx) {
        return Err(io::Error::other(format!("parallel applier file {ndx} already registered")));
    }
    files.insert(ndx, Arc::new(Mutex::new(FileSlot::new(writer, self.per_file_reorder_capacity))));
    Ok(())
}

pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>> {
    let ndx = ndx.into();
    let slot_arc = {
        let mut files = self.files.lock()
            .map_err(|_| io::Error::other("parallel applier file map poisoned"))?;
        files.remove(&ndx)
            .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?
    };
    // ... Arc::try_unwrap + into_inner unchanged ...
}
```

### After (DashMap, sketch only - BR-3j.c implements)

```rust
use dashmap::{DashMap, mapref::entry::Entry};

pub struct ParallelDeltaApplier {
    files: DashMap<FileNdx, Arc<Mutex<FileSlot>>>,
    per_file_reorder_capacity: usize,
    concurrency: usize,
}

fn slot_for(&self, ndx: FileNdx) -> io::Result<Arc<Mutex<FileSlot>>> {
    self.files.get(&ndx)
        .map(|r| Arc::clone(r.value()))
        .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))
}

pub fn register_file(&self, ndx: impl Into<FileNdx>, writer: Box<dyn Write + Send>)
    -> io::Result<()>
{
    let ndx = ndx.into();
    match self.files.entry(ndx) {
        Entry::Occupied(_) => Err(io::Error::other(format!(
            "parallel applier file {ndx} already registered"
        ))),
        Entry::Vacant(v) => {
            v.insert(Arc::new(Mutex::new(FileSlot::new(
                writer, self.per_file_reorder_capacity,
            ))));
            Ok(())
        }
    }
}

pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>> {
    let ndx = ndx.into();
    let (_k, slot_arc) = self.files.remove(&ndx)
        .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?;
    // ... Arc::try_unwrap + into_inner unchanged from today ...
}
```

The `Debug` impl loses its `unwrap_or(0)` rung because `DashMap::len()` does
not return a `Result`. Public type signatures stay the same; `register_file`,
`apply_chunk_parallel`, `apply_batch_parallel`, `bytes_written`, and
`finish_file` keep their existing names and return shapes.

## Gotchas for BR-3j.c / BR-3j.d

1. **Use `Entry`, not `contains_key` + `insert`.** DashMap's per-shard locks
   guarantee atomicity *within* a single call but not across two calls.
   Calling `files.contains_key(&ndx)` then `files.insert(ndx, ...)` opens a
   TOCTOU window where two concurrent `register_file`s for the same `ndx`
   can both observe the slot as empty. The `Entry::Occupied`/`Entry::Vacant`
   sketch above is the correct atomic pattern and preserves the
   "register exactly once" invariant tested by
   `double_registration_errors` at `parallel_apply.rs:629-637`.

2. **`Ref` guard holds the shard read lock - never call back into the map
   under it.** `self.files.get(&ndx)` returns a `Ref<FileNdx, _>` that holds
   a shard read lock for its lifetime. Clone the `Arc` and drop the guard
   immediately (the sketch's `map(|r| Arc::clone(r.value()))` does this).
   Holding a `Ref` while calling `self.files.insert(...)` or
   `self.files.remove(...)` on the same shard will deadlock; with the
   default 4xCPU shard count this is statistically rare but is the classic
   DashMap footgun and has bitten real callers.

3. **`Ref` is also `!Send`.** Do not store the `Ref` across an `.await` (we
   are not async today, but if BR-3j.d ever exposes the applier to an async
   wrapper this matters) or pass it across rayon work boundaries. Clone the
   `Arc` and pass that.

4. **`finish_file`'s `Arc::try_unwrap` invariant still holds.** The
   `ApplierStillReferenced` typed error
   (`parallel_apply.rs:62-80`) only fires when a worker has not dropped its
   `slot_for` clone. The DashMap swap does not change this:
   `DashMap::remove` returns the `Arc<Mutex<FileSlot>>` by value, identical
   to the current `HashMap::remove`. The test
   `finish_file_reports_typed_applier_still_referenced_with_strong_count`
   at `parallel_apply.rs:726-753` continues to assert the same shape.

5. **No outer mutex to poison.** `parking_lot_core` locks do not poison. The
   `"parallel applier file map poisoned"` error string at
   `parallel_apply.rs:325-327,438-440,469-471` becomes dead and should be
   deleted in the same change. The inner-`FileSlot`-poisoned arms
   (`parallel_apply.rs:84-90`, `ParallelApplyError::SlotPoisoned`) stay
   exactly as written - the per-file `Mutex` is still `std::sync::Mutex`.

6. **Tunability knob.** Default `4 * num_cpus().next_power_of_two()` shards
   may be wasteful when the file count is small (typical short transfer):
   each shard carries its own bucket array. If BR-3j.b shows this matters,
   prefer `DashMap::with_capacity_and_shard_amount(cap, shards)` where
   `shards = num_cpus().next_power_of_two()` (1x rather than 4x). Defer
   this tuning to bench evidence; do not preemptively cap.

7. **Dev-dep -> runtime-dep promotion.** `crates/engine/Cargo.toml:176`
   currently has `dashmap = { workspace = true }` under
   `[target.'cfg(unix)'.dev-dependencies]`. BR-3j.c must move it into the
   crate's runtime `[dependencies]` table (the engine crate is built on all
   platforms, so do not gate the runtime entry on `cfg(unix)`).

## MSRV check

`cargo info dashmap` against the registry on 2026-05-20:

```
dashmap = "7.0.0-rc2"   # latest available
rust-version: 1.65       # for the 6.x stable line in our lockfile
```

Workspace pin (`rust-toolchain.toml:2`): `1.88.0`. DashMap 6.1's required
toolchain (1.65) sits 23 minor versions below ours. **No MSRV blocker.**

Bumping to the 7.0 RC is out of scope for BR-3j and not recommended until
the daemon's `concurrent-sessions` usage migrates first; the existing
`dashmap = "6.1"` workspace pin is exactly what this audit recommends.

## Files referenced

- `crates/engine/src/concurrent_delta/parallel_apply.rs:248-489` - the
  applier and the field under audit.
- `crates/engine/src/concurrent_delta/types.rs:23-51` - `FileNdx` definition
  and `u32` conversions used as the map key.
- `crates/engine/Cargo.toml:176` - current dev-only `dashmap` dep on the
  engine crate.
- `crates/daemon/Cargo.toml:27,51` - `concurrent-sessions` feature gate that
  pulls DashMap at daemon runtime.
- `crates/daemon/src/daemon/connection_pool/pool.rs:11,36-62` and
  `crates/daemon/src/daemon/session_registry.rs:13,119-313` - existing
  production DashMap usage that BR-3j.c can use as an API reference.
- `crates/engine/benches/delete_plan_map_contention.rs:1-384` - sibling
  three-way bench harness; BR-3j.b should mirror its structure for the
  applier workload.
- `Cargo.toml:75-76` - workspace pin `dashmap = "6.1"`.
- `Cargo.lock` `[[package]] name = "dashmap"` - resolved at 6.1.0 with
  hashbrown 0.14.5.
- `rust-toolchain.toml:2` - workspace MSRV `1.88.0`.
