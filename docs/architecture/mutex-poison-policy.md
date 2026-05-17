# Mutex poison recovery policy

This document defines the engine's policy for reacting to a poisoned
`std::sync::Mutex` / `std::sync::RwLock`. It distils the per-site audit at
`docs/audits/mutex-poison-policy.md` (MPE-1 + MPE-2) and the helpers landed in
`crates/engine/src/util/poison.rs` (MPE-3) into one contract that new code must
follow.

Companion docs:

- `docs/audits/mutex-poison-policy.md` (MPE-1 + MPE-2 + MPE-9 - per-site
  classification)
- `crates/engine/src/util/poison.rs` (MPE-3 - `lock_or_recover`,
  `read_or_recover`, `write_or_recover`)
- `docs/architecture/drain-error-recovery.md` (companion drain contract for
  ATU-series fragile sites)

## Why this policy exists

When a thread holding a `Mutex` panics, the lock becomes poisoned. Every
subsequent `lock().unwrap()` / `lock().expect(...)` on the same mutex panics
again, turning a single-thread bug into a worker-pool-wide cascade. Some
state survives a panic (append-only counters, idempotent caches, bgid
free-lists); other state does not (in-flight wire frames, kernel-visible FFI
handles, parent-before-child traversal cursors). The right reaction depends
on what the lock guards, not on a one-size-fits-all rule.

The MPE audit found **368** lock sites across the workspace. The contract
below classifies every site into one of four buckets and gives the template
new code must use for each.

## The four cases

### RECOVERABLE - use `lock_or_recover`

The protected state is structurally valid after a panic. Continuing the
transfer is preferable to aborting the worker pool. Examples surfaced by
the audit: append-only event recorders, idempotent stat caches, bgid
free-lists with built-in deduplication, ssh `Child` handles in `Drop` paths.

Use `crates/engine/src/util/poison.rs::lock_or_recover` (or the `read`/`write`
analogues for `RwLock`).

### FATAL - keep `expect` with a `# Panics` rustdoc entry

Continuing risks corrupt wire output, lost wakeups, or use-after-free on
kernel-visible state. The audit identified 23 production-path FATAL sites
across the in-flight batch writer, the buffer-pool backpressure condvar,
the delete-traversal cursor, and the IOCP OVERLAPPED registry. Keep
`expect("...")`. Tighten the message to name the wire-stream invariant
being protected. Document the panic shape in rustdoc with a `# Panics`
section.

### TEST-ONLY - no policy

Single-threaded test fixtures, `RecordingFs` doubles, env-mutex serialisers
under `#[cfg(test)]`. The lock is never contended in the way the production
path is. A bare `expect("test mutex poisoned")` is acceptable. Migrating
these is a low-priority cleanup tracked under MPE-99 once `lock_or_recover`
is in every production crate.

### UNAUDITED - apply `lock_or_recover` by default

New code added before this contract is updated, or sites the audit did not
reach, default to `lock_or_recover`. The helper is the safer default
because it cannot cascade-poison a worker pool. Audit and reclassify in a
follow-up PR if the site turns out to be FATAL.

## Decision tree

"I am holding a `Mutex` and the surrounding code might panic. What do I do?"

```text
                  +----------------------------+
                  | Does the protected state   |
                  | survive a partial mutation |
                  | (counters, queues, caches, |
                  | append-only logs)?         |
                  +-------------+--------------+
                                |
              +-----------------+----------------+
              | yes                              | no
              v                                  v
   +------------------------+      +--------------------------+
   | Does the lock guard    |      | Does continuing risk:    |
   | a kernel-visible FFI   |      |   - torn wire frame      |
   | resource that another  |      |   - lost condvar wakeup  |
   | crate handed us?       |      |   - kernel-visible UAF   |
   +-----------+------------+      |   - ordering invariant   |
               |                   +-------------+------------+
       +-------+-------+                         |
       | yes           | no               +------+------+
       v               v                  | yes         | no
   FATAL          RECOVERABLE             v             v
   keep expect    use                  FATAL       Cannot reach
   + # Panics     lock_or_recover      keep        here in practice;
                                       expect      treat as
                                       + # Panics  RECOVERABLE
```

If the site is inside `#[cfg(test)]` or a test fixture, skip the tree -
classify as TEST-ONLY and move on.

If you cannot answer the first question with confidence, classify as
UNAUDITED and apply `lock_or_recover` until the next audit pass
reclassifies it.

## RECOVERABLE template

Import the helper and call it in place of `lock().unwrap()` /
`lock().expect(...)`:

```rust
use engine::util::poison::lock_or_recover;

fn record(&self, event: Event) {
    let mut log = lock_or_recover(&self.events);
    log.push(event);
}
```

For `RwLock`:

```rust
use engine::util::poison::{read_or_recover, write_or_recover};

fn lookup(&self, key: &Path) -> Option<Arc<Metadata>> {
    read_or_recover(&self.cache).get(key).cloned()
}

fn insert(&self, key: PathBuf, value: Arc<Metadata>) {
    write_or_recover(&self.cache).insert(key, value);
}
```

The helpers emit no `tracing` event today; future MPE work may add an
opt-in counter. Do not wrap them in additional logging at the call site -
the helper is the single source of truth so a CI lint can grep for it.

## FATAL template

Keep `expect`. Pick an invariant name that identifies the wire-stream
contract being protected. Document the panic in the function's rustdoc
with a `# Panics` section so callers know it is intentional.

```rust
/// Append a frame to the in-flight batch writer.
///
/// # Panics
///
/// Panics if the writer mutex is poisoned. A poisoned writer means a
/// previous frame was half-written; resuming would emit a torn batch
/// that no downstream rsync can replay. The transfer must abort.
fn write_frame(&self, frame: Frame) -> io::Result<()> {
    let mut writer = self
        .batch_writer
        .lock()
        .expect("BatchWriter wire-stream ordering invariant");
    writer.write_frame(frame)
}
```

Rules:

- The `expect` message names the invariant, not the site. Grep on the
  invariant name reveals every FATAL site that protects the same
  contract.
- The `# Panics` rustdoc describes why aborting is the only safe
  reaction. Callers reading the docs see that the panic is by design.
- Do not introduce a `lock_or_panic` wrapper. `expect("invariant")` is
  already grep-able and produces an identical backtrace.

## Audit counts

From `docs/audits/mutex-poison-policy.md` (368 total sites across the
workspace):

| Classification | Sites | Notes                                                             |
|----------------|-------|-------------------------------------------------------------------|
| FATAL          | 23    | Wire-stream / backpressure / IOCP / signature worker / cursor    |
| RECOVERABLE    | 28    | Caches, recorders, bgid free-list, env mutexes, ssh `Drop`       |
| TEST-ONLY      | 317   | Single-threaded fixtures across all crates                       |
| **Total**      | **368** |                                                                 |

Per-crate hotspots (combined single-line + multi-line passes):

| Crate       | Sites | Primary shape                                                       |
|-------------|-------|---------------------------------------------------------------------|
| `daemon`    | 125   | env mutex + chunked test harness                                    |
| `engine`    | 60    | batch writer (FATAL), delete cursor (FATAL), drain shard, recorder  |
| `cli`       | 49    | env mutex (RECOVERABLE prod) + frontend tests                       |
| `core`      | 39    | compress env mutex + client tests                                   |
| `transfer`  | 25    | reader/writer/receiver tests                                        |
| `protocol`  | 25    | multiplex reader tests                                              |
| `fast_io`   | 14    | bgid free-list (RECOVERABLE) + IOCP registry (FATAL)                |
| `metadata`  | 8     | id-lookup tests                                                     |
| `flist`     | 7     | batched stat cache (RECOVERABLE)                                    |
| `platform`  | 6     | privilege + env test mutex                                          |
| `rsync_io`  | 5     | ssh `Child` handle (RECOVERABLE, reference pattern)                 |
| `signature` | 1     | worker receiver (FATAL)                                             |
| `embedding` | 1     | test                                                                |
| `checksums` | 1     | test                                                                |
| `branding`  | 1     | test                                                                |
| `bandwidth` | 1     | test                                                                |

Top single-file remediation target:
`crates/engine/src/local_copy/context_impl/options.rs` (7 FATAL batch-writer
sites sharing one `expect` message; tightening them lands the wire-stream
invariant naming convention for the rest of the engine to copy).

## Cross-references

- `docs/audits/mutex-poison-policy.md` - the per-site classification this
  policy distils.
- `crates/engine/src/util/poison.rs` - the helper implementation, module
  docs, and unit tests.
- `docs/architecture/drain-error-recovery.md` - the ATU-series companion
  for `Arc::try_unwrap` drain failures. Drain code that also touches a
  `Mutex` must use `lock_or_recover` AND surface drain failures as typed
  errors per that contract.
- `crates/rsync_io/src/ssh/connection.rs` and
  `crates/fast_io/src/refs_detect.rs` - pre-existing RECOVERABLE call
  sites that match the helper recipe; treat them as worked examples.

## Promotion path

1. **MPE-1 + MPE-2 + MPE-9** (landed) - the per-site audit at
   `docs/audits/mutex-poison-policy.md`.
2. **MPE-3** (landed) - the `lock_or_recover`, `read_or_recover`,
   `write_or_recover` helpers in `crates/engine/src/util/poison.rs`.
3. **MPE-4..MPE-8** (planned) - per-file replacements inside
   `crates/engine/src/delete/**`. MPE-4 replaces 5 FATAL plan-map sites
   with tightened `expect` messages, MPE-5 covers `delete/context.rs`,
   MPE-6/7 wrap the recorder/scripted-fs test doubles in
   `lock_or_recover`, MPE-8 is the audit pass that confirms every
   `engine/src/delete/**` site uses one of the two recipes.
4. **MPE-10** (planned) - stress test that panics a worker thread mid-lock
   on every RECOVERABLE site and asserts the surrounding pool keeps
   serving subsequent transfers (no cascade-poisoning).
5. **MPE-11** (this document) - the contract that ties (1) (2) and the
   future (3) (4) together.
6. **Future** - this contract becomes a CI lint. Any new `lock().unwrap()`
   or `lock().expect(...)` outside `#[cfg(test)]` that does not either
   call `lock_or_recover` or sit next to a `# Panics` rustdoc entry fails
   review.
