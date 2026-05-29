# Remaining shared_ring callers after the per-thread migration (IUR-6.a)

Tracking: IUR-6.a. This is an inventory record. No `.rs` files change. It
takes a post-migration snapshot of every caller that still touches a
shared (non-per-thread) io_uring ring, classifies each as deliberately
kept shared or as a candidate for removal, and hands the resulting
surface to the removal task IUR-6.b. The companion design notes are:

- `docs/design/io-uring-shared-ring-audit.md` - IUR-1 caller-surface
  audit; the pre-migration inventory this record refreshes.
- `docs/design/iur-2-per-thread-rings.md` - IUR-2 hybrid layout that
  chose which factories move to per-thread rings and which stay shared.
- `docs/design/iur-3f-shared-rings-decision.md` - IUR-3.f decision
  record that formalised the kept-shared categories.
- `docs/design/iur-3g-zerocopysender-migration-deferred.md` - IUR-3.g
  deferral of the `ZeroCopySender` ring.

## Purpose

After the IUR-3 migration, the hot factories no longer serialise on a
single ring. This audit pins down what is left: it lists each remaining
construction or use of a shared ring (anything not on the IUR-3.a
per-thread primitive), states what the caller does, and classifies it
`KEEP-SHARED` (intentional, per IUR-3.f / IUR-3.g) or
`CANDIDATE-FOR-REMOVAL` (leftover surface IUR-6.b can retire or rewire).
The classification feeds IUR-6.b, which is itself gated on the IUR-5
benchmark decision and is out of scope here.

## Scope note: this is an internal concurrency concern

The shared-vs-per-thread ring topology is an oc-rsync-internal io_uring
submission-queue concurrency matter. It does not touch the rsync wire
protocol, the file list, the delta stream, or any byte exchanged with a
peer. Upstream rsync has no io_uring; there is no upstream behaviour to
mirror and nothing here changes interop. The only correctness axis is
that each ring keeps its single-owner / single-submitter invariant.

## Background: what IUR-3 moved and what it kept

IUR-3 migrated the three high-frequency, multi-producer factories to a
lazy thread-local `OnceLock`-style ring via the IUR-3.a primitive
`per_thread_ring::with_ring` (`crates/fast_io/src/io_uring/per_thread_ring.rs`):

| Factory | File | Migrated to | Subtask |
|---------|------|-------------|---------|
| `file_writer` | `crates/fast_io/src/io_uring/file_writer.rs:75,133,175,228,288` | per-thread `with_ring` | IUR-3.b (#4804) |
| `file_reader` | `crates/fast_io/src/io_uring/file_reader.rs:69,120` | per-thread `with_ring` | IUR-3.c (#4807) |
| `socket_writer` | `crates/fast_io/src/io_uring/socket_writer.rs:94,145` | per-thread `with_ring` | IUR-3.d (#4806) |
| `BgidLease` (per-ring buffer ids) | `crates/fast_io/src/io_uring/bgid_lease.rs` | per-thread BGE-4 lease | IUR-3.e (#4811) |

IUR-3.f (`docs/design/iur-3f-shared-rings-decision.md`) deliberately KEPT
two categories on a shared / single-owner ring, and IUR-3.g deferred a
third:

- One-shot capability probes (`linkat`, `renameat2`, `statx`) - one
  throwaway ring per probe site, result cached in a process-wide
  `OnceLock<bool>`.
- The disk-commit singleton `IoUringDiskBatch` - one ring for the life
  of the session, owned by the disk-commit thread.
- The `ZeroCopySender` ring (`Arc<Mutex<RawIoUring>>`) - deferred behind
  IUS-8, opt-in feature, no production caller (IUR-3.g).

## Inventory table

Columns: `file:symbol` | what it does | topology today | classification.

Classification key:
- `KEEP-SHARED` - intentional shared / single-owner ring, ratified by
  IUR-3.f or IUR-3.g; not a removal blocker.
- `CANDIDATE-FOR-REMOVAL` - dormant or unwired shared-ring surface that
  IUR-6.b can retire or rewire once IUR-5 decides the rollout.

| file:symbol | what it does | topology today | classification |
|-------------|--------------|----------------|----------------|
| `crates/fast_io/src/io_uring/shared_ring.rs:SharedRing` (type + `try_new:126`, `new:136`, `submit_read:233`, `submit_poll_write:260`, `submit_send:287`, `submit_and_wait:308`, `reap:318`) | One io_uring ring co-locating a reader fd and a writer fd in one session; the original single-ring topology the migration replaced. | One `RawIoUring` per `SharedRing` instance; no `Arc<Mutex>` wrapper today. | CANDIDATE-FOR-REMOVAL |
| `crates/fast_io/src/io_uring_common.rs:SharedRingConfig` (`:193`) | Plain-data config struct for `SharedRing`. | data only | CANDIDATE-FOR-REMOVAL (follows `SharedRing`) |
| `crates/fast_io/src/io_uring_common.rs:SharedCompletion` (`:269`) | CQE demux enum returned by `SharedRing::reap`. | data only | CANDIDATE-FOR-REMOVAL (follows `SharedRing`) |
| `crates/fast_io/src/io_uring_common.rs:OpTag` | 8-bit-tag + 56-bit-op-id `user_data` layout, re-exported via `shared_ring.rs:85`. | data only | KEEP-SHARED-ADJACENT - the demux scheme is reusable by any multi-op CQ; `cancel.rs:388-389` references it by comment only. IUR-6.b should preserve `OpTag` even if `SharedRing` is retired. |
| `crates/fast_io/src/io_uring/session_pool.rs:SessionRingPool` / `RingLease` | Bounded fleet of `N` rings behind per-slot mutexes, round-robin acquire. The session-mutex shared-pool primitive. | `N` rings, each behind `std::sync::Mutex` | CANDIDATE-FOR-REMOVAL - no production caller; only tests (`session_pool.rs:518,537,583,595,604`) and doctests/tests in `linked_chain.rs`. |
| `crates/fast_io/src/io_uring/session_pool.rs:ThreadLocalRingPool` / `ThreadLocalRingLease` | One ring per OS thread, lazily built; the per-thread fallback target named by IUR-2. | thread-local | KEEP-SHARED-ADJACENT - this is the per-thread side of the pool, not a shared ring. No production caller yet (tests only at `session_pool.rs:626,652,689,708,727,750`), but it is the intended migration target, not removal surface. Flagged for IUR-6.b awareness, not removal. |
| `crates/fast_io/src/io_uring/disk_batch.rs:IoUringDiskBatch` (`:45`, `new:70`) | Single ring reused across the disk-commit phase; batches writes to rotating files. `!Send + !Sync`, single-submitter. | one `RawIoUring` per session, disk-commit thread owns it | KEEP-SHARED (IUR-3.f section 3) |
| `crates/transfer/src/disk_commit/thread.rs:try_create_disk_batch` (`:77,84,89`), consumed in `disk_commit/process.rs:42,160,298` and `disk_commit/writer.rs:148` | The only production caller of `IoUringDiskBatch`; spawns one per session. | single-owner | KEEP-SHARED (IUR-3.f section 3) |
| `crates/fast_io/src/io_uring/linkat.rs:linkat_supported` (`:89`, ring at `:113,181`) | One-shot `IORING_OP_LINKAT` probe; throwaway `IoUring::new(2)`, cached in `OnceLock<bool>` (`:49`). | one ring per process, dropped after probe | KEEP-SHARED (IUR-3.f section 2) |
| `crates/fast_io/src/io_uring/renameat2.rs:renameat2_supported` (`:66`, ring at `:76,165`) | One-shot `IORING_OP_RENAMEAT` probe; cached in `OnceLock<bool>` (`:56`). | one ring per process, dropped after probe | KEEP-SHARED (IUR-3.f section 2) |
| `crates/fast_io/src/io_uring/statx.rs:statx_supported` (`:117`, ring at `:138,223`) | One-shot `IORING_OP_STATX` probe; cached in `OnceLock<bool>` (`:71`). | one ring per process, dropped after probe | KEEP-SHARED (IUR-3.f section 2) |
| `crates/fast_io/src/io_uring/socket_reader.rs:IoUringSocketReader::*` (ring at `:32` via `config.build_ring()`) | Socket reader; builds its own per-instance ring. One reader per session. | one ring per reader instance | KEEP-SHARED (IUR-2 section 1.1; "shared (one reader per session)", not migrated) |
| `crates/fast_io/src/io_uring/send_zc.rs:ZeroCopySender` (`ring: Arc<Mutex<RawIoUring>>` `:284`, `new:309`, `from_shared_ring:345`) | Opt-in zero-copy socket sender behind the `iouring-send-zc` feature; owns its own ring or accepts a caller-supplied shared one. | `Arc<Mutex<RawIoUring>>` | KEEP-SHARED (IUR-3.g deferred; no production caller, default-off feature). `from_shared_ring:345` has no production caller. |

### Transient rings (not shared_ring callers, listed for completeness)

These build an ephemeral ring scoped to a single call. They are neither
per-thread nor shared infrastructure, so they are not migration or
removal surface; noted so a future reader does not mistake them for
shared-ring callers.

| file:symbol | what it does |
|-------------|--------------|
| `crates/fast_io/src/io_uring/statx.rs:submit_statx_batch_io_uring` (`:294`, ring at `:305`) | Builds a per-call ring sized to the batch, submits all paths as independent SQEs, drops the ring on return. |

## Non-Linux stub (noted separately)

`crates/fast_io/src/io_uring_stub/shared_ring.rs:SharedRing` mirrors the
Linux type on non-Linux targets or when the `io_uring` cargo feature is
off. Every constructor returns `None` / `io::ErrorKind::Unsupported` and
`reap` returns an empty vector; the struct holds only `_private: ()`.
The whole `io_uring_stub/` tree (re-exported at `io_uring_stub/mod.rs:48,66,100`)
is a compile-time API mirror with no live ring. When IUR-6.b retires or
rewires the Linux `SharedRing`, the stub follows mechanically and is not
an independent removal blocker - it carries no behaviour beyond returning
the unsupported sentinels.

## Removal readiness conclusion

Findings that bound the IUR-6.b surface:

1. **The `SharedRing` type has zero production callers.** A
   `grep -rn 'SharedRing|shared_ring' --include='*.rs'` across `crates/`
   resolves every non-doc use to the type's own module, the non-Linux
   stub, the re-export chain (`io_uring/mod.rs:191`, `lib.rs:306`), and
   the test/bench files
   (`crates/fast_io/tests/io_uring_shared_ring.rs`,
   `crates/fast_io/tests/io_uring_mmap_pressure.rs`,
   `crates/fast_io/benches/iouring_per_file_vs_shared.rs`). No code in
   `engine`, `transfer`, `daemon`, `core`, or anywhere outside `fast_io`
   constructs or borrows a `SharedRing`. This matches IUR-1
   (`io-uring-shared-ring-audit.md` section 3.1: "`SharedRing` is
   **dormant infrastructure**") and the IUR-2 scope note
   (`iur-2-per-thread-rings.md` section 9: "IUR-1 confirmed it has zero
   production callers ... IUR-6 decides whether to retire or rewire it").
   The migration did not introduce any new `SharedRing` caller, so the
   removal surface is exactly the dormant type plus its data structs and
   test/bench fixtures.

2. **`SessionRingPool` is also dormant.** Like `SharedRing`, it has no
   production caller; only tests and `linked_chain` examples exercise it.
   It is a `CANDIDATE-FOR-REMOVAL` peer of `SharedRing`.

3. **Clean removal candidates (do not block transfers):**
   `SharedRing`, `SharedRingConfig`, `SharedCompletion`,
   `SessionRingPool` / `RingLease`. Retiring these is zero-risk to the
   live data path because nothing on the live data path uses them.
   `OpTag` and `ThreadLocalRingPool` are adjacent and should be preserved
   (the demux scheme and the per-thread pool are forward-looking, not
   removal surface).

4. **Removal blockers - the kept-shared rings that IUR-6.b must NOT
   touch:** the disk-commit singleton (`IoUringDiskBatch` +
   `transfer/src/disk_commit/`), the one-shot probes
   (`linkat`/`renameat2`/`statx` `*_supported`), the per-session
   `socket_reader` ring, and the deferred `ZeroCopySender`
   (`Arc<Mutex<RawIoUring>>`). Each has a ratified decision record
   (IUR-3.f for probes + disk-commit, IUR-2 1.1 for socket_reader,
   IUR-3.g for `ZeroCopySender`) and should only be reopened on the
   trigger criteria in those records, not as part of a routine removal
   sweep.

5. **Gating.** The actual removal / rewire work (IUR-6.b) is gated on the
   IUR-5 benchmark decision (`iur-2-per-thread-rings.md` section 7 / the
   bench at `crates/fast_io/benches/iouring_per_file_vs_shared.rs:264-297`,
   which IUR-4 / IUR-5 extend). This inventory does not pre-empt that
   decision; it only fixes the surface so IUR-6.b knows precisely what is
   removable (item 3), what to keep (item 4), and what is forward-looking
   (`OpTag`, `ThreadLocalRingPool`).

## Cross-references

- `docs/design/io-uring-shared-ring-audit.md` - IUR-1 pre-migration
  caller-surface audit.
- `docs/design/iur-2-per-thread-rings.md` sections 1.1, 7, 9 - the
  hybrid layout and the "`SharedRing` retire-or-rewire" deferral to
  IUR-6.
- `docs/design/iur-3f-shared-rings-decision.md` - kept-shared probes +
  disk-commit.
- `docs/design/iur-3g-zerocopysender-migration-deferred.md` - deferred
  `ZeroCopySender` ring.
- `crates/fast_io/src/io_uring/per_thread_ring.rs` - the IUR-3.a
  per-thread primitive the migrated factories now use.
- `crates/fast_io/benches/iouring_per_file_vs_shared.rs:264-297` - the
  IUR-5 bench cell that gates IUR-6.b.
