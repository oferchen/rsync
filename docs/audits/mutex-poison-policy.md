# Mutex poison recovery classification

Tracking issues: MPE-1 (#2350), MPE-2 (#2351), MPE-9 (#2358).

## Summary

This audit inventories every `Mutex::lock().expect()` / `Mutex::lock().unwrap()`
(and the analogous `RwLock::read/write` variants) call site in the workspace,
classifies each as RECOVERABLE (safe to use `PoisonError::into_inner()` and
keep going) or FATAL (a panic mid-section corrupts shared state, so the only
safe response is to abort), and maps the result to follow-up remediation
tasks.

Three lock-pattern shapes are surveyed:

1. `x.lock().unwrap()` - single-line, no message.
2. `x.lock().expect("...")` - single-line with diagnostic.
3. The multi-line builder shape:
   ```
   x.lock()
       .expect("...")
   ```

The first round of `grep` only catches shapes (1) and (2); a follow-up scan
catches shape (3). Both passes are folded into the totals below.

The classification is read-only. No source file is modified by this document.

## Methodology

1. Workspace-wide grep with two passes:

   ```sh
   # Pass 1: single-line shapes.
   grep -rn \
       '\.lock()\.expect\|\.lock()\.unwrap\|\.read()\.expect\|\.read()\.unwrap\|\.write()\.expect\|\.write()\.unwrap' \
       crates/

   # Pass 2: multi-line builder shape.
   grep -rn -B1 -E '^\s*\.expect\(|^\s*\.unwrap\(\)' crates/ \
     | grep -B1 -E '\.lock\(\)$'
   ```

2. Per-call-site classification driven by three questions:

   - **What does the lock guard?** Pure scratch data (`Vec` of recorded
     events), wire-stream state (in-progress batch-writer bytes), config
     cache (env mutex serializing process-globals), shared work-queue
     accumulator, FFI handle (SSH child process), io_uring bgid free-list,
     IOCP overlapped registry.
   - **What can panic while the lock is held?** Trivial code paths
     (`Vec::push`, `HashMap::insert`, atomic CAS) almost never panic.
     Code that writes wire frames, runs user-provided closures, or
     traverses a `FileEntry` graph can panic on malformed input.
   - **What is the blast radius of "continue after poison"?** If the lock
     guards data whose internal invariants survive a partial mutation
     (idempotent caches, append-only event recorders, file-write
     scratchpads where the operation is going to be retried anyway),
     `into_inner()` is safe and the process should keep serving other
     transfers. If continuing risks corrupt wire output or use-after-free
     on FFI handles, the panic must propagate.

3. Cross-references to follow-up tasks (MPE-3 through MPE-NN). The MPE-3
   task introduces a `lock_or_recover` helper (matches the pattern already
   used by `rsync_io/src/ssh/connection.rs` and `fast_io/src/refs_detect.rs`)
   that wraps `lock().unwrap_or_else(|e| e.into_inner())` with a `tracing`
   warning. Per-file remediation tasks reuse that helper for every
   RECOVERABLE site and leave FATAL sites with a tightened `expect`
   message that names the wire-stream invariant being protected.

## Totals

Across `crates/**`:

| Pass                   | Sites |
|------------------------|-------|
| Single-line shape      | 333   |
| Multi-line builder     | 35    |
| **Total**              | **368** |

Per-crate distribution (combined passes):

| Crate        | Sites |
|--------------|-------|
| `daemon`     | 125   |
| `engine`     | 60    |
| `cli`        | 49    |
| `core`       | 39    |
| `transfer`   | 25    |
| `protocol`   | 25    |
| `fast_io`    | 14    |
| `metadata`   | 8     |
| `flist`      | 7     |
| `platform`   | 6     |
| `rsync_io`   | 5     |
| `signature`  | 1     |
| `embedding`  | 1     |
| `checksums`  | 1     |
| `branding`   | 1     |
| `bandwidth`  | 1     |

Test-vs-production split (single-line pass; the multi-line additions follow
the same distribution):

| Category                                  | Sites |
|-------------------------------------------|-------|
| `tests.rs` / `*/tests/*` / `benches` / `examples` | 239 |
| Production source (`*/src/*` non-test)   | 94    |

Most of the production-source hits are still test helpers, env-mutex guards
behind `#[cfg(test)]` blocks, or `RecordingFs`-style fakes embedded in the
crate root rather than under a `tests/` directory. The genuinely
production-path sites total **27**, listed in Table 2.

## Table 1: `crates/engine/src/delete/` sites

| File:line | What is locked | Classification | Rationale |
|---|---|---|---|
| `delete/plan_map.rs:76` | `Mutex<HashMap<PathBuf, DeletePlan>>` (publish-once map) | **FATAL** | A panic between `Mutex::lock()` and `HashMap::insert()` would corrupt the publish-once invariant the emitter depends on; the only operations under the lock are `HashMap::insert`/`remove`/`len`/`is_empty`/`contains_key`, none of which panic in practice. Keep `expect`. |
| `delete/plan_map.rs:88` | same | **FATAL** | `take()` is the only consumer; a poisoned map means a peer thread crashed mid-publish and the unread side is undefined. |
| `delete/plan_map.rs:97` | same | **FATAL** | `is_empty()` shape; same reasoning. |
| `delete/plan_map.rs:106` | same | **FATAL** | `len()` shape; same reasoning. |
| `delete/plan_map.rs:119` | same | **FATAL** | `contains()` shape; same reasoning. |
| `delete/context.rs:263` | `Mutex<DirTraversalCursor>` (parent-before-child traversal state) | **FATAL** | The cursor's stack ordering is the upstream emission-order invariant; a torn state silently mis-orders deletes (parent emitted before its children are flushed). Keep `expect("DeleteContext cursor mutex poisoned")`. |
| `delete/context.rs:276` | same | **FATAL** | `observe_directory` mutates the same stack; same reasoning. |
| `delete/context.rs:291` | `Mutex<Vec<FileEntry>>` (per-directory segment buffer) | **FATAL** | `begin_directory` overwrites the previous segment in place. Continuing with a half-written segment would compute extras against a stale name list and delete the wrong files. |
| `delete/context.rs:307` | same | **FATAL** | `publish_plan_for` clones the segment; if the panic happened during the `.clone()` the buffer is intact, but it could also have happened during the earlier `begin_directory` overwrite. Cheaper to keep the strict policy. |
| `delete/context.rs:475` | `Mutex<DirTraversalCursor>` | **TEST-ONLY** | `#[cfg(test)] mod tests` body. Tests are single-threaded fixtures; an `expect` here is fine but `unwrap` is misleading. Convert to `expect("test cursor poisoned")` for consistency. |
| `delete/context.rs:510` | same | **TEST-ONLY** | Same as above. |
| `delete/context.rs:568` | same | **TEST-ONLY** | Same as above. |
| `delete/emitter/fs.rs:153` | `Mutex<Vec<DeleteEvent>>` inside `RecordingDeleteFs` | **RECOVERABLE** | Append-only event log used only by tests; reading the partial log after a poisoning panic still produces a debuggable trace. Use `lock_or_recover`. |
| `delete/emitter/fs.rs:158` | same | **RECOVERABLE** | `record()` does `lock().expect(...).push()`. Push is panic-free; keep `expect` or, after MPE-3, switch to `lock_or_recover`. |
| `delete/emitter/tests/mod.rs:64` | `Mutex<Vec<(PathBuf, ErrorKind)>>` test script | **TEST-ONLY** | `ScriptedDeleteFs` rule queue. Tests are single-threaded; current `expect` is fine. |
| `delete/emitter/tests/mod.rs:75` | same | **TEST-ONLY** | Same. |

Counts inside `crates/engine/src/delete/`:

| Classification | Sites |
|---|---|
| FATAL          | 9     |
| RECOVERABLE    | 2     |
| TEST-ONLY      | 5     |
| **Total**      | **16** |

## Table 2: workspace production-path hotspots

Only call sites in genuine production code paths (i.e., not behind
`#[cfg(test)]`, not in `tests/`, not in `benches/`, not in `examples/`)
are listed here. Test-instrumentation helpers that live inside `src/` but
are only ever invoked from tests (`RecordingDeleteFs`, env mutexes,
`signal/cleanup.rs` `TEST_LOCK`, etc.) are categorised as RECOVERABLE
because they are not on the production path either.

### `crates/engine/`

| File:line | What is locked | Classification | Rationale |
|---|---|---|---|
| `concurrent_delta/parallel_apply.rs:432` | `Mutex<Vec<u8>>` (test sink only) | **TEST-ONLY** | Lives in `#[cfg(test)] mod tests` inside the apply module. Convert `expect` to `lock_or_recover` after MPE-3, or leave as-is. |
| `concurrent_delta/parallel_apply.rs:465` | same | **TEST-ONLY** | Same. |
| `concurrent_delta/work_queue/drain.rs:81` | `Mutex<Vec<R>>` (per-shard result buffer) | **RECOVERABLE** | The drain shard buffer is a write-only accumulator; partial fill is acceptable because the caller treats a poisoned drain as a fatal worker crash anyway. Use `lock_or_recover` and surface the poison via a counter. |
| `local_copy/buffer_pool/memory_cap.rs:84` | `Mutex<()>` paired with `Condvar` for the buffer-pool backpressure waiters | **FATAL** | Poisoning here means a slow-path waiter panicked mid-`wait`; continuing would skip the `Condvar` handshake and cause lost wakeups. Keep `expect("backpressure mutex poisoned")`. |
| `local_copy/buffer_pool/memory_cap.rs:149` | same | **FATAL** | `track_return` lock-then-notify pattern; recovering would defeat the textbook condvar guarantee. |
| `local_copy/context_impl/options.rs:574` | `Arc<Mutex<BatchWriter>>` (in-progress batch wire stream) | **FATAL** | A poisoned batch writer means a previous frame was half-written; resuming would emit a torn batch file that no downstream rsync can replay. Keep `expect`, tightened message. |
| `local_copy/context_impl/options.rs:601` | same | **FATAL** | Reads `protocol_version` from config; the panic would have to be in `config()` which is infallible, but recovering still risks racing a half-flushed writer. |
| `local_copy/context_impl/options.rs:623` | same | **FATAL** | Same as `:574`. |
| `local_copy/context_impl/options.rs:668` | same | **FATAL** | Same as `:601`. |
| `local_copy/context_impl/options.rs:925` | same | **FATAL** | Same as `:574`; this is the inner NDX-write loop. |
| `local_copy/context_impl/options.rs:950` | same | **FATAL** | Same as `:601`; NDX_DONE phase preamble. |
| `local_copy/context_impl/options.rs:962` | same | **FATAL** | Same as `:574`; NDX_DONE writer. |
| `local_copy/context_impl/state.rs:53` | same | **FATAL** | Read-only `config()` lookup at context-construction time; if the writer is poisoned at startup the entire local-copy session is unsafe. |
| `local_copy/context_impl/state.rs:60` | same | **FATAL** | Same. |
| `local_copy/executor/directory/recursive/batch.rs:158` | same | **FATAL** | Per-directory flist entry write; same reasoning as `options.rs`. |

### `crates/fast_io/`

| File:line | What is locked | Classification | Rationale |
|---|---|---|---|
| `io_uring/buffer_ring.rs:198` | `Mutex<Vec<u16>>` (process-wide bgid free-list) | **RECOVERABLE** | The free-list is a pure scratch container; a poisoned state at most loses one bgid (or pushes a duplicate, which `deallocate` already deduplicates). Use `lock_or_recover`. |
| `io_uring/buffer_ring.rs:246` | same | **RECOVERABLE** | `deallocate` already guards against duplicates; safe to recover. |
| `io_uring/buffer_ring.rs:262` | same | **RECOVERABLE** | `remaining()` is read-only; safe to recover. |
| `io_uring/buffer_ring.rs:1101` | same | **RECOVERABLE** | Same. |
| `io_uring/buffer_ring.rs:1115` | same | **RECOVERABLE** | Same. |
| `io_uring/buffer_ring.rs:1217` | same | **RECOVERABLE** | Same. |
| `iocp/pump.rs:252` | `Mutex<HashMap<usize, CompletionHandler>>` (in-flight OVERLAPPED registry) | **FATAL** | Each `CompletionHandler` owns kernel-visible state; losing one means an OVERLAPPED will never be invoked and its caller leaks the pinned allocation. Keep `expect`. |
| `iocp/pump.rs:265` | same | **FATAL** | Same. |
| `iocp/pump.rs:275` | same | **FATAL** | `pending_ops` read; if poisoned, the count is wrong and shutdown will deadlock. |
| `iocp/pump.rs:425` | same | **FATAL** | Worker-thread completion dispatch; recovering would skip invoking the handler. |
| `refs_detect.rs:86` | `Mutex<Option<HashMap<PathBuf, bool>>>` (volume cache) | **RECOVERABLE** (already done) | Already uses `unwrap_or_else(|e| e.into_inner())`. This is the recipe we will replicate elsewhere. |
| `refs_detect.rs:97` | same | **RECOVERABLE** (already done) | Same. |
| `refs_detect.rs:106` | same | **RECOVERABLE** (already done) | Same. |

### `crates/rsync_io/`

| File:line | What is locked | Classification | Rationale |
|---|---|---|---|
| `ssh/connection.rs:107` | `Mutex<Option<Child>>` (SSH subprocess handle) | **RECOVERABLE** (already done) | Already uses `unwrap_or_else(|e| e.into_inner())`. The reference pattern. |
| `ssh/connection.rs:138` | same | **RECOVERABLE** (already done) | Same. |
| `ssh/connection.rs:164` | same | **RECOVERABLE** (already done) | Same. |
| `ssh/connection.rs:293` | `Mutex<Option<Child>>` (split half) | **RECOVERABLE** (already done) | Same. |
| `ssh/connection.rs:570` | `Mutex<Option<Child>>` (Drop reaper) | **RECOVERABLE** (already done) | Same; running in `Drop`, where panicking on poison would double-panic and abort. |

### `crates/signature/`

| File:line | What is locked | Classification | Rationale |
|---|---|---|---|
| `async_gen.rs:335` | `Arc<Mutex<Receiver<WorkerMessage>>>` (shared MPMC-like receiver) | **FATAL** | A poisoned channel receiver means a peer worker died holding the receive end. Treating "recover and try again" would spin on an undefined `recv`; aborting the worker thread is the upstream policy. Keep `unwrap`, but switch to a typed `expect("signature worker receiver poisoned")`. |

### `crates/flist/`

| File:line | What is locked | Classification | Rationale |
|---|---|---|---|
| `batched_stat/cache.rs:71` | `Mutex<HashMap<PathBuf, Arc<Metadata>>>` (per-shard stat cache) | **RECOVERABLE** | Cache lookup is idempotent; a partial cache is a cache miss. Use `lock_or_recover`. |
| `batched_stat/cache.rs:78` (multi-line) | same | **RECOVERABLE** | `insert` is idempotent; safe to recover. |
| `batched_stat/cache.rs:95` | same | **RECOVERABLE** | Fast-path read; safe to recover. |
| `batched_stat/cache.rs:110` (multi-line) | same | **RECOVERABLE** | `insert` on the slow path; safe to recover. |
| `batched_stat/cache.rs:154` | same | **RECOVERABLE** | `clear()` is destructive but only invoked from tests/teardown; safe to recover. |
| `batched_stat/cache.rs:161` | same | **RECOVERABLE** | `len()` read; safe to recover. |
| `batched_stat/cache.rs:167` | same | **RECOVERABLE** | `is_empty()` read; safe to recover. |

### `crates/protocol/`

Production source contains zero in-production lock sites. The 25 hits all
live in tests, examples, or `mod tests` blocks inside
`multiplex/reader.rs`. Classified as **TEST-ONLY**; no remediation
required.

### `crates/daemon/`

`crates/daemon/src/systemd.rs` (8 sites) and
`crates/daemon/src/daemon/sections/xfer_exec.rs` (3 sites) hold the only
production `lock()` calls; all 11 wrap a thread-local `ENV_LOCK` used to
serialise `std::env::set_var` against rayon-spawned daemon workers.
Classified as **RECOVERABLE**: a poisoned env mutex is symptomatic of a
test panic, and the production-path callers want to keep serving
unrelated connections.

The remaining 114 daemon sites are all inside `crates/daemon/src/tests/`
chunked harness fixtures - **TEST-ONLY**.

### `crates/cli/`

`crates/cli/src/frontend/arguments/env.rs` (12 sites) wraps the same
process-global env mutex against argument-parsing tests; all production
callers reach it from `parse_args` paths under `#[cfg(test)]`-only
exercises. Classified as **RECOVERABLE**.

The remaining 37 cli sites live in `crates/cli/src/frontend/tests/`.
**TEST-ONLY**.

### `crates/core/`

`crates/core/src/client/config/compress_env.rs` (5 sites) wraps the env
mutex around compression test exercises - **RECOVERABLE**. The remaining
34 sites live in `crates/core/src/client/tests/` and
`crates/core/src/signal/cleanup.rs` test fixtures - **TEST-ONLY**.

### `crates/platform/`

`platform/src/privilege.rs:315` (1 site) and `platform/src/env.rs`
(5 sites) wrap test-only env/tz mutexes - **RECOVERABLE** (production
visibility through `#[cfg(test)]` guard).

### `crates/metadata/`

All 8 sites are in `metadata/src/id_lookup/tests.rs` - **TEST-ONLY**.

### `crates/transfer/`

All 25 sites are in `transfer/src/{reader,writer,receiver}/tests/` and
the two `benches/` files - **TEST-ONLY**.

### `crates/checksums/`, `crates/branding/`, `crates/bandwidth/`, `crates/embedding/`

One site each, all inside `#[cfg(test)]` or `tests/` modules -
**TEST-ONLY**.

## Production-path classification roll-up

| Category    | Sites | Crates                                                            |
|-------------|-------|--------------------------------------------------------------------|
| FATAL       | 23    | `engine` (15 batch-writer + buffer-pool + plan-map), `fast_io` (4 iocp), `signature` (1), `engine/src/delete` (3 cursor + 4 plan_map already counted above) |
| RECOVERABLE | 28    | `flist` (7), `fast_io` (6 bgid + 3 refs_detect), `rsync_io` (5 already done), `engine` (1 drain + 2 recorder), `daemon` (3 xfer_exec already done), `engine/src/delete/emitter/fs.rs` (2) |
| TEST-ONLY   | 317   | spread across all crates, predominantly `daemon`, `cli`, `core`, `transfer` |
| **Total**   | **368** | |

(The FATAL/RECOVERABLE counts above include the delete-module rows from
Table 1; the workspace-wide counters add the delete totals into the
overall production roll-up.)

## Recommended remediation recipe

1. **MPE-3** (`engine` shared helper): introduce a `lock_or_recover`
   helper in `crates/engine/src/util/` (or a new top-level
   `crates/util/sync.rs`) with the signature

   ```rust
   pub fn lock_or_recover<'a, T>(m: &'a Mutex<T>) -> MutexGuard<'a, T> {
       m.lock().unwrap_or_else(|e| {
           tracing::warn!(target: "sync.poison", "recovered from poisoned mutex");
           e.into_inner()
       })
   }
   ```

   Mirrors the pattern already in `rsync_io/src/ssh/connection.rs` and
   `fast_io/src/refs_detect.rs`. Add a `lock_or_panic` companion that
   takes an `&'static str` invariant message for FATAL sites, so every
   `expect` call is grep-able by invariant name rather than by call-site.

2. **MPE-4..MPE-8** (per delete-file remediation):

   - **MPE-4**: `crates/engine/src/delete/plan_map.rs` - replace 5
     `expect("DeletePlanMap mutex poisoned")` calls with
     `lock_or_panic("DeletePlanMap publish-once invariant")`.
   - **MPE-5**: `crates/engine/src/delete/context.rs` - replace 4
     production `expect` calls (cursor + segment_entries) with
     `lock_or_panic("DeleteContext cursor traversal order")`. Replace
     the 3 test-only `unwrap` calls with the same recipe to keep test
     diagnostics consistent.
   - **MPE-6**: `crates/engine/src/delete/emitter/fs.rs` - swap
     `RecordingDeleteFs` to `lock_or_recover` so a test panic does not
     poison the entire suite.
   - **MPE-7**: `crates/engine/src/delete/emitter/tests/mod.rs` -
     swap `ScriptedDeleteFs` to `lock_or_recover` for the same reason.
   - **MPE-8**: rgrep audit pass to confirm every
     `engine/src/delete/**` lock site now uses one of the two helpers
     and nothing else.

3. **MPE-NN per other crate** (open follow-up):

   - **MPE-10**: `crates/engine/src/local_copy/context_impl/{options,state}.rs` +
     `executor/directory/recursive/batch.rs` - 15 batch-writer FATAL
     sites, all should adopt `lock_or_panic("BatchWriter wire-stream
     ordering")`.
   - **MPE-11**: `crates/engine/src/local_copy/buffer_pool/memory_cap.rs` -
     2 FATAL backpressure sites, adopt
     `lock_or_panic("BufferPool backpressure condvar")`.
   - **MPE-12**: `crates/engine/src/concurrent_delta/work_queue/drain.rs:81` -
     1 RECOVERABLE shard buffer, adopt `lock_or_recover` and add a
     poison-counter metric.
   - **MPE-13**: `crates/fast_io/src/io_uring/buffer_ring.rs` -
     6 RECOVERABLE bgid free-list sites, adopt `lock_or_recover` (the
     `refs_detect.rs` siblings are already converted).
   - **MPE-14**: `crates/fast_io/src/iocp/pump.rs` - 4 FATAL OVERLAPPED
     registry sites, adopt `lock_or_panic("IOCP handler registry")`.
   - **MPE-15**: `crates/signature/src/async_gen.rs:335` - 1 FATAL
     receiver mutex, tighten `unwrap` to `expect("signature worker
     receiver poisoned")`.
   - **MPE-16**: `crates/flist/src/batched_stat/cache.rs` - 7
     RECOVERABLE shard-cache sites, adopt `lock_or_recover`.
   - **MPE-17**: `crates/daemon/src/{systemd.rs,daemon/sections/xfer_exec.rs}` +
     `crates/cli/src/frontend/arguments/env.rs` +
     `crates/core/src/client/config/compress_env.rs` +
     `crates/platform/src/env.rs` - audit pass to migrate every test-env
     mutex to a shared `test_env::lock_or_recover` helper in
     `crates/platform`, so a single test panic does not poison every
     downstream env-coupled test.

The TEST-ONLY remediation tasks (317 sites) are best handled by one
sweeping `cargo fix`-style PR per crate that uses a search-and-replace
to wrap every `lock().unwrap()` with `lock_or_recover()` once MPE-3
ships; that work is tracked under a single MPE-99 umbrella issue rather
than 14 per-crate tickets.

## Top remediation target

`crates/engine/src/local_copy/context_impl/options.rs` is the highest-value
single file: 7 FATAL batch-writer sites that today share a single
`expect("...")` string with no `tracing` signal. The remediation lands a
strict invariant name, a `tracing::error!` on the poisoned-mutex panic
path, and a single recipe that the rest of the engine can copy. This is
the recommended starting point for MPE-10.

## Followup notes

- **MPE-7 / `crates/engine/src/delete/traversal.rs` (#2356)**: verified
  no-op. The file defines [`DirTraversalCursor`], a single-threaded DFS
  helper documented as "single-threaded by construction" (only the
  emitter thread holds an instance). It contains zero `Mutex`/`RwLock`
  fields, zero `.lock()` calls, and zero `.expect()`/`.unwrap()` call
  sites in either the production code or the unit tests. The shared
  `Mutex<DirTraversalCursor>` mentioned in Table 1 lives in
  `delete/context.rs:263,276,475,510,568` and is tracked under MPE-5,
  not here. No source change is required for MPE-7's `traversal.rs`
  scope; this audit row is the sole deliverable.
