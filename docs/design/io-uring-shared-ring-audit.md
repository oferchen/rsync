# io_uring `SharedRing` caller-surface audit (IUR-1)

Tracking task: **IUR-1**. Predecessor design notes:

- `docs/design/iouring-per-thread-rings.md` - the per-thread alternative
  whose recommendation IUR-2 will refine.
- `docs/design/iouring-session-ring-pool.md` - the round-robin pool that
  ships today as `SessionRingPool` and `ThreadLocalRingPool`.
- `docs/design/io-uring-bgid-namespace.md` and the BGE-* tasks (#2293-#2299) -
  the bgid free-list and recycling pool.
- `docs/design/mmap-vs-sqpoll-decision.md` and SMR-3c (#2290) -
  the defensive SQPOLL-vs-mmap fallback.

This is a pure caller-surface audit. No `.rs` files change. The follow-up
IUR-2 design picks the per-thread layout; IUR-3 implements it.

## 1. Scope

The originating premise (memory note: "shared_ring concurrency
bottleneck") is that `SharedRing` is wrapped in `Arc<Mutex<...>>` and
serialises submission. Section 2 confirms that framing is not literally
true today, then catalogs the actual caller surface so IUR-2 can pick a
per-thread layout with full context.

The audit covers:

- Every import and use of `SharedRing`, `SharedRingConfig`,
  `SharedCompletion`, and `OpTag` in `crates/fast_io/src/io_uring/`.
- Every downstream `fast_io::` import in `crates/engine/` and
  `crates/transfer/` that reaches into the io_uring submission path.
- The Linux io_uring callers that are not on `SharedRing` but compete
  for the same kernel resources (per-file rings, `ZeroCopySender`'s
  separate `Arc<Mutex<RawIoUring>>`, `SessionRingPool`,
  `ThreadLocalRingPool`).

Out of scope: the IOCP path, the splice / sendfile zero-copy paths,
non-io_uring fast_io helpers (`copy_file_range`, `DirSandbox`,
`recursive_unlinkat`).

## 2. Topology today

### 2.1 `SharedRing` is single-owner

`crates/fast_io/src/io_uring/shared_ring.rs:98-111` defines
`SharedRing` as a plain struct owning one `io_uring::IoUring`. Its
`submit_*` methods take `&mut self`
(`shared_ring.rs:233`, `:260`, `:287`), `submit_and_wait` takes
`&mut self` (`shared_ring.rs:308`), and `reap` takes `&mut self`
(`shared_ring.rs:318`). The type carries no interior mutability and is
neither `Sync` nor wrapped in a lock by any production caller.

A repo-wide search for `Mutex<SharedRing>` returns zero matches. The
"Arc&lt;Mutex&gt; over a single ring" framing of the IUR memory note
refers to the **proposed** topology in
`docs/design/iouring-session-ring-pool.md:37-59`, not shipped code.
The single shipped `Arc<Mutex<RawIoUring>>` instance is on
`ZeroCopySender` (see 2.4), not on `SharedRing`.

### 2.2 `SharedRing` is not on a hot transfer path today

The only production import is the re-export in
`crates/fast_io/src/io_uring/mod.rs:178` (`pub use shared_ring::{OpTag,
SharedCompletion, SharedRing, SharedRingConfig};`) and the parallel
re-export from `crates/fast_io/src/lib.rs:296-298`. No `SharedRing::*`
constructor call exists outside of:

- `crates/fast_io/src/io_uring/shared_ring.rs` itself (definition).
- `crates/fast_io/src/io_uring/cancel.rs:388-389` (comment only,
  referencing the private `OpTag` scheme; no constructor call).
- `crates/fast_io/tests/io_uring_shared_ring.rs:50,88,179` (tests).
- `crates/fast_io/tests/io_uring_mmap_pressure.rs:110,163` (tests).

Confirmation that nothing in `engine` or `transfer` reaches
`SharedRing`: a `grep -rn 'SharedRing\|shared_ring' --include='*.rs'
crates/engine crates/transfer` returns zero matches. Downstream io_uring
usage in those crates is exclusively through high-level helpers like
`fast_io::read_file_with_io_uring`
(`crates/engine/src/concurrent_delta/strategy.rs:371`).

### 2.3 The actually-shipping per-file ring pattern

What ships today is **one `RawIoUring` per submitter**, each owned by
the submitting type:

| Site | File:line | Lifetime | Frequency |
|------|-----------|----------|-----------|
| `IoUringReader::open` | `file_reader.rs:65` | per-file | per `open()` |
| `IoUringWriter::create` | `file_writer.rs:59,85,179` | per-file | per `create()` |
| `IoUringWriter` Direct-IO ctor | `file_writer.rs:470` | per-file | per ctor |
| `IoUringSocketReader::new` | `socket_reader.rs:32` | per-socket | per ctor |
| `IoUringSocketWriter::new` | `socket_writer.rs:64` | per-socket | per ctor |
| `IoUringDiskBatch::new` | `disk_batch.rs:71` | per-session | once |
| `ZeroCopySender::new` | `send_zc.rs:316` | per-session | once |
| `LinkAt::probe` | `linkat.rs:113,181` | per-process probe | once |
| `RenameAt::probe` | `renameat2.rs:76,165` | per-process probe | once |
| `Statx::probe` | `statx.rs:138,223,305` | per-process probe | once |
| `cancel.rs` test ring | `cancel.rs:325` | test only | n/a |

`SessionRingPool` (`session_pool.rs:145-220`) and
`ThreadLocalRingPool` (`session_pool.rs:332-422`) already exist as
opt-in primitives. The thread-local pool is the per-thread-ring
mechanism IUR-2 will compose with; today no production submitter has
migrated to it. The session pool wraps each ring in
`Vec<Mutex<RawIoUring>>` (`session_pool.rs:146`); the thread-local
pool stores the ring in `RefCell<Option<RawIoUring>>` inside a
thread-local (`session_pool.rs:299-300`).

### 2.4 The one `Arc<Mutex<RawIoUring>>` in production

`ZeroCopySender` (`send_zc.rs:283-292`, feature `iouring-send-zc`)
holds `ring: Arc<Mutex<RawIoUring>>`. The mutex is acquired around
every `try_send_zc` call (`send_zc.rs:419-422`, `:436-439`). This is
not `SharedRing` and is not used by any other submitter; the
`Arc<Mutex<...>>` exists so multiple `ZeroCopySender`s built via
`from_shared_ring` (`send_zc.rs:345`) can share a session-scoped ring.
In practice no production call site invokes `from_shared_ring`, so the
mutex is uncontested.

This is the surface IUR-2 has to either fold into the per-thread design
or document as deliberately shared.

## 3. Caller surface catalog

### 3.1 `SharedRing` constructors and submit calls (production)

| Caller (fn / location) | Crate | Frequency | Submitted op | Hot path? |
|------------------------|-------|-----------|--------------|-----------|
| `SharedRing::try_new` (`shared_ring.rs:126`) | fast_io | per session pairing | none directly | n/a (constructor) |
| `SharedRing::new` (`shared_ring.rs:136`) | fast_io | per session pairing | none directly | n/a (constructor) |
| `SharedRing::submit_read` (`shared_ring.rs:233`) | fast_io | per file read chunk | `IORING_OP_READ` (or `READ` via fixed-file slot) | yes - bulk basis read on receive |
| `SharedRing::submit_poll_write` (`shared_ring.rs:260`) | fast_io | per writer drain | `IORING_OP_POLL_ADD` (POLLOUT) | yes - upstream `io.c:perform_io` analogue |
| `SharedRing::submit_send` (`shared_ring.rs:287`) | fast_io | per writer payload | `IORING_OP_SEND` | yes - mirrors upstream wire send |
| `SharedRing::submit_and_wait` (`shared_ring.rs:308`) | fast_io | per batch of SQEs | drives any queued SQEs | yes |
| `SharedRing::reap` (`shared_ring.rs:318`) | fast_io | per batch | drains CQEs | yes |

**Production caller count: zero.** The constructors and submit methods
have no production call sites; they are exercised only by the tests
listed in 2.2 and the bench at
`crates/fast_io/benches/iouring_per_file_vs_shared.rs` (which models
the shared-ring topology against the per-file baseline).

This is the load-bearing finding of IUR-1: `SharedRing` is **dormant
infrastructure**. Migrating it off the (notional) shared mutex is
zero-risk; the open question is whether IUR-2 should retire it
(option A in section 6) or wire it into the receiver write path on a
per-thread basis (option B).

### 3.2 `SharedRingConfig`, `SharedCompletion`, `OpTag` users

| Symbol | Imported by | Purpose |
|--------|-------------|---------|
| `SharedRingConfig` | `io_uring_common.rs:188-195`, `lib.rs:296` re-export, tests | plain-data ring config |
| `SharedCompletion` | `io_uring_common.rs`, `lib.rs:296` re-export, tests | CQE demux enum |
| `OpTag` | `io_uring/shared_ring.rs:85` re-export, `io_uring/mod.rs:178`, `lib.rs`, tests | 64-bit user_data tag layout |

`OpTag`'s 8-bit-tag + 56-bit-op-id layout
(`shared_ring.rs:30-42`) is the canonical demux scheme. Any per-thread
ring that hosts multiple op kinds on one CQ will reuse it. The cancel
module references it by comment only (`cancel.rs:388-389`); the cancel
SQE path builds raw `user_data` bit patterns to avoid colliding with
the `SharedRing` namespace.

### 3.3 Downstream io_uring entry points (engine, transfer)

`crates/engine/` and `crates/transfer/` do not touch `SharedRing`,
`RawIoUring`, or any io_uring SQ/CQ primitive. The only ingress is:

| Helper | Call site | Lifetime |
|--------|-----------|----------|
| `fast_io::read_file_with_io_uring` | `engine/src/concurrent_delta/strategy.rs:371` | per basis-file read |
| `fast_io::write_file_with_io_uring` | reached via `IoUringOrStdWriter` in `transfer/src/transfer_ops/response.rs:15` | per output file |
| `fast_io::FileReader` / `FileWriter` traits | `transfer/src/map_file/mmap.rs:16`, `transfer/src/transfer_ops/response.rs:15` | per file |
| `fast_io::PlatformCopy` | `engine/src/local_copy/options/platform_copy.rs:10` | per copy op |

Each of these resolves to a per-file `RawIoUring` constructed inside
the factory (`file_factory.rs:117,241,256`), never to `SharedRing`.
The factory path is the one IUR-2 must reroute through the per-thread
pool to capture the win.

### 3.4 The contention model

Because `SharedRing` has no production caller, **there is no
`SharedRing` contention to measure today**. The contention surface
IUR-2 has to dissolve is:

1. **Per-file `io_uring_setup(2)` churn.** Every output file builds a
   new ring (`file_writer.rs:59,85,179`) and tears it down on drop.
   On a 100K small-file workload this is 100K setup syscalls plus
   100K teardowns. The bench at
   `benches/iouring_per_file_vs_shared.rs:1-60` was built to measure
   exactly this cost.
2. **`ZeroCopySender::ring` mutex.** One `lock()` per `send_zc` call
   (`send_zc.rs:419-422`, `:436-439`). Uncontended in production
   today because `from_shared_ring` has no caller, but the lock
   acquire is on the critical path of every wire send.
3. **`SessionRingPool` per-slot mutex.** `acquire()` (`session_pool.rs:200-207`)
   takes a `MutexGuard` for the full submit-and-reap cycle. The
   contention is bounded by `min(available_parallelism(), 16)`
   slots (`session_pool.rs:62`, `:114`).

Each of these has a **single-thread holder pattern**: the lock is
acquired, a small batch of SQEs is pushed, `submit_and_wait` blocks
until kernel completion, the CQs are drained, the lock drops. There
is no SQE batch-fill phase distinct from submit; the kernel transition
happens immediately after each push burst. A per-thread ring removes
the lock in all three cases (per-thread storage for case 2 requires
`!Send`, which `ZeroCopySender` is already by construction of its
`!Sync` ring).

## 4. BGID lifecycle interaction (BGE-4)

BGE-4 (`docs/design/io-uring-bgid-namespace.md`, task #2296) added a
process-global free-list backing the bgid allocator:

- `BgidAllocator::allocate` (`buffer_ring/allocator.rs:193`+, called
  from `buffer_ring/mod.rs:371`) drains a `Mutex<Vec<u16>>` free-list
  (`buffer_ring/allocator.rs:100-101`) before incrementing the
  `NEXT_BGID` `AtomicU32` (`buffer_ring/allocator.rs:39`).
- `BgidAllocator::deallocate` (called from `buffer_ring/mod.rs:385`
  on construction failure and `:571` on `BufferRing::Drop`) returns
  the id to the free-list. The `Drop` site is the dominant deallocator
  on long-running daemons.
- Counters `PEAK_USED` (`allocator.rs:48`), `BGID_EXHAUSTED_COUNT`
  (`allocator.rs:56`), and `BGID_FALLBACK_WARNED` (`allocator.rs:66`)
  are process-global atomics. `bgid_peak_used()`, `bgid_inflight()`,
  `bgid_exhausted_count()` re-export them through `lib.rs:297-298`.

**Per-thread interaction.** `BufferRing` and its bgid are tied to one
`RawIoUring` at registration time (`buffer_ring/mod.rs:238`,
`RegisteredBufferGroup::try_new` referenced from `shared_ring.rs:172`
and `send_zc.rs:318`). Pinned buffer pages cannot move between rings -
the kernel records them against the registering ring's fd. Per-thread
rings each need their own `BufferRing` and their own bgid.

Namespace pressure stays low: 64 rayon workers * 16 buffers per ring
at `for_large_files()` defaults = 1024 bgids, 1.5 % of the 65 536-id
space. The 50 %-occupancy warning (`allocator.rs:78-85`) has wide
headroom. The free-list `Mutex<Vec<u16>>` is acquired only during
allocate/deallocate (push/pop), never during submit/reap. Per-thread
rings keep that order intact and add no new cross-thread contention
on bgids. Per-thread bgid pools are a possible BGE follow-up if the
free-list mutex appears on a flame graph; today it does not.

## 5. SQPOLL interaction (SMR-3c)

SMR-3c (`docs/design/mmap-vs-sqpoll-decision.md`, task #2290) shipped a
per-ring SQPOLL fallback driven by `IoUringConfig::mmap_basis_active`
(`io_uring_common.rs:111`). `build_ring()` (`io_uring/config.rs:346-373`)
refuses SQPOLL when `mmap_basis_active` is set and falls back to a
regular ring; on `EPERM` / `ENOMEM` (missing `CAP_SYS_NICE`) it falls
back the same way. The fallback is recorded in `SQPOLL_FALLBACK`
(`config.rs:53`).

**Per-thread implications.**

- **One SQPOLL kthread per ring.** Per kernel docs (5.13+), each
  io_uring with `IORING_SETUP_SQPOLL` spawns its own kthread unless
  `IORING_SETUP_ATTACH_WQ` is used to share an existing wq with
  another ring (the io-uring crate v0.7 exposes this through
  `Builder::setup_attach_wq`). N per-thread rings + SQPOLL = N
  kthreads. On a 64-core box this is 64 kernel threads dedicated to
  SQ polling. Kernels handle this (kthread overhead is small) but it
  inflates the `ps` count and consumes one SQPOLL idle slot per ring
  (`sqpoll_idle_ms = 1000` default, `io_uring_common.rs:132`).
- **Recommendation for IUR-2.** Per-thread rings should default
  SQPOLL **off** unless `attach_wq` is wired to consolidate the
  kthreads onto a single wq. The current `IoUringConfig::sqpoll`
  default is `false` (`io_uring_common.rs:130`), so this is the
  status quo; the design only needs to guard against future
  callers blindly enabling SQPOLL across all per-thread rings.
- **mmap interaction is unchanged.** `mmap_basis_active` is a
  per-config flag; per-thread rings each consult their own config
  and each defensively disables SQPOLL when paired with an
  `MmapStrategy` basis. No new hazard.

The shipped `SessionPoolConfig::flags` and `sqpoll_idle_ms`
(`session_pool.rs:71-93`) already plumb SQPOLL through to per-ring
construction. `ThreadLocalRingPool::new` (`session_pool.rs:348`)
reuses the same config path, so per-thread SQPOLL is feasible without
new plumbing.

## 6. Possible per-thread layouts

IUR-1 enumerates candidates without picking one; IUR-2 picks based on
the bench at `benches/iouring_per_file_vs_shared.rs` and any new
measurements.

### 6.1 Pure thread-local (one ring per Rust thread, lazy init)

The shipped `ThreadLocalRingPool` (`session_pool.rs:332-422`) already
implements this. Thread-local storage holds a `RefCell<Option<RawIoUring>>`
keyed by `(pool_id, ThreadId)`. Lazy init on first `acquire()`.

- Pros: zero lock on the submit/reap path. Already implemented,
  including a clone-shares-per-thread-ring test
  (`session_pool.rs:721-737`). Re-entrant acquire correctly returns
  `None` (`session_pool.rs:684-700`).
- Cons: ring lifetime is "until thread exit"; rings created on
  short-lived threads churn `io_uring_setup(2)` per thread. The
  thread-local `RefCell` is `!Send`, so callers cannot move the lease
  across threads (this is enforced by the lease wrapper). Rayon
  worker threads outlive transfers, so the ring is reused; ad-hoc
  `thread::spawn` workers each pay the setup cost on first use.

Call sites that change: every `file_writer.rs:59,85,179`,
`file_reader.rs:65`, `socket_reader.rs:32`, `socket_writer.rs:64`
construction becomes "acquire from `ThreadLocalRingPool`, build SQEs
against the leased ring". `IoUringDiskBatch::new` (`disk_batch.rs:71`)
is unaffected because the disk-commit thread already owns its ring
for the session lifetime.

### 6.2 Per-rayon-worker (matches the existing pool topology)

A variant of 6.1 that attaches the per-thread ring to rayon worker
threads specifically, via
`rayon::ThreadPoolBuilder::start_handler` (referenced from
`docs/design/iouring-per-thread-rings.md:142-148`).

- Pros: every rayon worker gets a ring eagerly; setup cost is paid
  once at pool startup, never on the hot path. Bound on ring count
  is `rayon::current_num_threads()` (typically `min(cores, 16)`).
- Cons: rings built before any io_uring work is dispatched pin
  kernel resources for the whole pool lifetime even if the workload
  is CPU-bound. Couples io_uring lifetime to rayon pool lifetime,
  reducing flexibility for non-rayon submitters (the disk-commit
  thread already opts out by owning its own ring).

Call sites that change: same as 6.1, plus a single attachment hook
in the rayon pool init at
`crates/fast_io/src/parallel.rs:159` and `:217`. Submitters look up
the rayon-attached ring via `rayon::current_thread_index()`.

### 6.3 Per-pipeline-stage (separate rings for read/write/network)

Split the receiver pipeline into three rings: one for basis-file
reads, one for output-file writes, one for socket I/O. Each ring is
owned by the thread that drives that stage (basis reader, disk
committer, network reader/writer).

- Pros: matches the existing pipeline topology
  (`crates/transfer/src/pipeline/spsc.rs`). The disk-commit thread
  already owns its ring (`disk_batch.rs:46`, single-threaded by
  construction). Each stage gets exactly one ring; resource use is
  predictable and minimal.
- Cons: doesn't help if a single stage runs across multiple workers
  (e.g., rayon-parallel basis reads). Falls back to per-thread (6.1)
  or shared-with-lock for parallel stages.

Call sites that change: receiver/sender main pipelines wire each stage
into its dedicated ring. Disk-commit thread is already this shape; the
work is to extend the pattern to the reader and network stages.

### 6.4 Hybrid (per-thread for hot path, shared for rare path)

Per-thread rings for `file_writer`, `file_reader`, `socket_writer`.
Keep the bounded `SessionRingPool` for low-frequency operations
(`linkat`, `renameat2`, `statx` metadata fast paths, BGE bgid
registration sequences). `ZeroCopySender` either gets its own
per-thread ring or joins the shared pool depending on bench results.

- Pros: minimises kernel pages pinned for short-lived metadata
  operations (one shared ring rather than N per-thread rings for
  ops that happen once per file or once per directory).
  `statx::probe`-style one-shot operations don't need a per-thread
  ring at all.
- Cons: two pools to reason about. The decision of which path a
  given op goes through becomes a per-call-site policy.

Call sites that change: hot path (3-4 sites) goes to per-thread;
metadata path (linkat/renameat/statx, currently one-shot
`io_uring::IoUring::new(2)` calls in their probe paths) goes to the
shared session pool. `IoUringDiskBatch` continues to own its private
ring.

### 6.5 What stays untouched under any layout

`SharedRing` itself (no callers), `BgidAllocator` free-list,
`SQPOLL_FALLBACK` atomic, `is_io_uring_available()` cached probe,
and the high-level `fast_io::read_file_with_io_uring` /
`write_file_with_io_uring` helpers downstream crates call - the
per-thread plumbing stays inside `fast_io`.

## 7. Recommendation for IUR-2

The per-thread primitive already exists; the gap is **caller
migration**, not new infrastructure. The promising layout is **6.4
hybrid**: route the three high-frequency factory sites (`file_writer`,
`file_reader`, `socket_writer`) through `ThreadLocalRingPool`, keep
the one-shot metadata probes and the disk-commit ring as-is, and let
`ZeroCopySender` continue with its own `Arc<Mutex<RawIoUring>>` until
the bench shows the lock as a bottleneck. This avoids the kernel-page
inflation of 6.1 / 6.2 while capturing the per-file-setup win that
`benches/iouring_per_file_vs_shared.rs` measures.

The retire-or-wire decision for `SharedRing` itself can wait: it has
zero callers and is tracked by IUR-6.
