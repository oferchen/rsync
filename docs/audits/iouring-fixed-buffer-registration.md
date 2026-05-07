# io_uring fixed-buffer registration: deeper drill-down

Tracking issue: oc-rsync #2118.

This audit is a deeper drill-down on the registration mechanics
already covered by PR #3754
([`docs/audits/io-uring-fixed-buffer-audit.md`](io-uring-fixed-buffer-audit.md)).
PR #3754 maps the call sites, opcode usage, lifecycle, and pool sizing.
This document narrows the scope to the registration moment itself, the
pinned-memory accounting that the kernel does on our behalf, the failure
surface the registration syscall exposes, the fallback behaviour that
hides each of those failures, and the gaps left open by the prior
write-up.

Sibling audits worth reading first if the context is unfamiliar:
[`docs/audits/io-uring-fixed-buffer-audit.md`](io-uring-fixed-buffer-audit.md)
(map of call sites and opcodes),
[`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md)
(telemetry and resize policy),
[`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md)
(disjoint PBUF_RING namespace),
[`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md)
(provided-buffer-ring, kernel 5.19+).

## TL;DR

Registration is **eager, per-owner, at construction time**, not lazy
and not per-transfer. Every successful `IoUringReader::open`,
`IoUringWriter::create` / `from_file` / `with_ring` /
`create_with_size`, and `SharedRing::new` performs one
`IORING_REGISTER_BUFFERS` syscall before returning, allocating
`registered_buffer_count` page-aligned buffers and pinning them in
kernel memory. The default footprint is `8 buffers x 64 KiB =
512 KiB` per owner, charged against `RLIMIT_MEMLOCK` for the calling
user. On any registration failure (low `RLIMIT_MEMLOCK`, kernel slot
limit, seccomp denial, `ENOMEM`, etc.) the owner's
`registered_buffers` field stays at `None` and every subsequent SQE
falls back to the unfixed `IORING_OP_READ` / `IORING_OP_WRITE`
opcode against caller memory. There is no retry, no resize, and no
deferred re-registration today. Two real gaps remain for follow-up:
no pre-flight `RLIMIT_MEMLOCK` probe, and no telemetry distinguishing
"registration disabled by config" from "registration attempted and
failed". The remainder of this document develops each point.

## 1. Where the registration syscall is issued

`IORING_REGISTER_BUFFERS` is invoked from exactly one production
function in the workspace:

- `RegisteredBufferGroup::new`
  (`crates/fast_io/src/io_uring/registered_buffers.rs:251-345`)
  performs the syscall via the
  `io_uring::Submitter::register_buffers` wrapper at
  `crates/fast_io/src/io_uring/registered_buffers.rs:307`. The wrapper
  ultimately reaches `io_uring_register(fd, IORING_REGISTER_BUFFERS,
  iovecs, count)` at the syscall boundary.

`RegisteredBufferGroup::try_new`
(`crates/fast_io/src/io_uring/registered_buffers.rs:352`) is the
public best-effort entry point that swallows any error from `new` and
returns `Option<Self>`. Every owner constructs the group through
`try_new`, so a registration failure is silent at the owner level: the
owner stores `registered_buffers = None` and walks the unfixed code
path forever after. This is the central design choice: registration
is opportunistic, never mandatory.

The five owners that call `try_new` are:

| Owner | File:line | Construction context |
|---|---|---|
| `IoUringReader::open` | `crates/fast_io/src/io_uring/file_reader.rs:73-81` | Per file open |
| `IoUringWriter::create` | `crates/fast_io/src/io_uring/file_writer.rs:56-64` | Per file create |
| `IoUringWriter::from_file` | `crates/fast_io/src/io_uring/file_writer.rs:83-91` | Per `File` wrap |
| `IoUringWriter::with_ring` | `crates/fast_io/src/io_uring/file_writer.rs:118` | Per `writer_from_file` (hard-codes `count = 8`) |
| `IoUringWriter::create_with_size` | `crates/fast_io/src/io_uring/file_writer.rs:143-151` | Per pre-sized create |
| `SharedRing::new_inner` | `crates/fast_io/src/io_uring/shared_ring.rs:267-275` | Per shared ring (registers, never consumes) |

Every owner that has a `register_buffers: bool` config flag honours
it. The single exception is `with_ring` at
`crates/fast_io/src/io_uring/file_writer.rs:118`, which calls
`try_new` unconditionally and hard-codes `count = 8`. PR #3754
recorded this as a minor wart; this audit confirms it, but adds the
following observation that #3754 missed: `with_ring` also ignores the
`register_buffers` flag entirely, so a caller that has explicitly
disabled buffer registration in `IoUringConfig` still pays the
allocation cost when the writer is constructed via
`writer_from_file`. The mitigation is the same one-line change in the
`writer_from_file_with_depth` parameter list
(`crates/fast_io/src/io_uring/mod.rs:166-218`); the wider call out
here is that disabling registration via config is a partial control
today.

## 2. Registration timing: at ring init, not per-transfer, not lazy

Three timing models are possible for fixed-buffer registration:

| Model | Description |
|---|---|
| **At ring init** | One `register_buffers` call as part of the constructor that builds the ring. Buffers live for the ring's lifetime. |
| **Per-transfer** | Allocate and register the buffers needed for the next file, deregister on file close. |
| **Lazy** | Defer registration until the first I/O attempt, then memoise. |

oc-rsync uses the **at-ring-init** model exclusively. The owner's
constructor builds the `RawIoUring` at `config.build_ring()`, then
immediately calls `RegisteredBufferGroup::try_new(&ring,
config.buffer_size, config.registered_buffer_count)` before returning
`Self`. The buffer group is stored as an `Option<RegisteredBufferGroup>`
field and is never re-registered, resized, or replaced for the
remainder of the owner's lifetime. The relevant invariants:

1. **No re-registration.** The codebase contains no call to
   `RegisteredBufferGroup::new` after the constructor. There is no
   `replace`, no `set_register`, no shrink-and-grow path. A surveyed
   ripgrep over the whole repo for `register_buffers(` returns the
   single library-internal call at `registered_buffers.rs:307` and
   nothing else.
2. **No deregistration on idle.** `Drop::drop`
   (`crates/fast_io/src/io_uring/registered_buffers.rs:453-476`)
   deallocates the user-side memory but never calls
   `unregister_buffers`. The kernel releases the pinning as part of
   the ring fd close that runs during the owner's `Drop` chain.
3. **No reuse across owners.** Two `IoUringWriter` instances do not
   share a `RegisteredBufferGroup`; each constructs its own. The
   `SharedRing` does own a group, but the doc-comment at
   `crates/fast_io/src/io_uring/shared_ring.rs:320-322` explicitly
   says that the registered-buffer fast path is reserved for a
   future batched session-level submitter that owns its own slot
   bookkeeping. PR #3754 calls this Section 5a and labels it a
   deliberate gap; this audit re-reads the code and confirms the
   group is allocated, pinned, and never checked out anywhere in
   the production tree.

The choice is ergonomic. At-ring-init keeps the
`RegisteredBufferGroup` field statically typed as a plain `Option`
behind the owner's mutex-of-self model; per-transfer would force a
deregister/register cycle on every file boundary, paying the
allocation and pinning cost twice for no benefit on a workload like
oc-rsync's where the ring is per-file already. Lazy registration is
not implemented and would only matter if buffer registration were
expected to fail commonly enough that paying the syscall up front
hurt the median open path; the failure surface (Section 5) is too
narrow to justify lazy registration.

## 3. Memory cost and the kernel side of registration

`IORING_REGISTER_BUFFERS` walks the supplied `iovec` array and pins
each contiguous range of pages via `get_user_pages_fast` /
`pin_user_pages_fast` (kernel-version dependent; see
`io_uring/io_uring.c::io_sqe_buffers_register` on a current tree).
Pinning has three observable effects for oc-rsync:

### 3a. Per-owner pinned-memory footprint

Each `RegisteredBufferGroup` allocates `count` page-aligned buffers,
where each buffer is `buffer_size` rounded up to a multiple of
`page_size()` (sysconf `_SC_PAGESIZE`, fallback 4 KiB; see
`crates/fast_io/src/io_uring/registered_buffers.rs:271-272`,
`479-487`). The default is `count = 8`, `buffer_size = 64 KiB`,
giving `512 KiB` per owner. The presets at
`crates/fast_io/src/io_uring/config.rs:344-373` produce:

| Preset | `count` | `buffer_size` | Pinned per owner |
|---|---|---|---|
| `default()` | 8 | 64 KiB | 512 KiB |
| `for_large_files()` | 16 | 256 KiB | 4 MiB |
| `for_small_files()` | 8 | 16 KiB | 128 KiB (rounded to 8 x 4 KiB pages = 32 KiB; see Section 3c) |

Each owner is per file or per ring. Two writers running in parallel
hold two groups; a daemon processing 200 concurrent transfers in
the large-file preset would pin `200 * 4 MiB = 800 MiB` of kernel
memory before any data has flowed. There is no per-process
deduplication: groups do not share pages. This is a real ceiling on
concurrency under low `RLIMIT_MEMLOCK`.

### 3b. `RLIMIT_MEMLOCK` accounting

Pinned pages charge against the calling user's `RLIMIT_MEMLOCK`
(`ulimit -l`, `/proc/<pid>/limits`). The default on most modern
distributions is `64 KiB` to `8 MiB` per user, well below the
`for_large_files()` preset. The kernel returns `ENOMEM` from
`IORING_REGISTER_BUFFERS` when the request would push the
user-locked memory above the limit, which surfaces as
`io::ErrorKind::Other` from
`crates/fast_io/src/io_uring/registered_buffers.rs:312-314` and is
swallowed by `try_new`.

oc-rsync does not pre-flight `RLIMIT_MEMLOCK` before calling
`register_buffers`. There is no `getrlimit(RLIMIT_MEMLOCK)` probe
anywhere in the workspace (verified via ripgrep on `RLIMIT_MEMLOCK`
and `getrlimit`). Failure is detected reactively: the syscall
returns an error, `try_new` returns `None`, and the owner runs
forever in the unfixed-opcode mode. This is deliberate (the
fallback is designed to be transparent) but it does mean a low
`RLIMIT_MEMLOCK` is invisible at the API surface today; nothing
distinguishes "the user has locked memory headroom and we did not
register" from "we tried, the kernel said no, here's the unfixed
path".

### 3c. Page-alignment rounding

`buffer_size` is rounded up to a page boundary at
`crates/fast_io/src/io_uring/registered_buffers.rs:272` via
`buffer_size.next_multiple_of(page_size())`. The configured value is
not authoritative; `RegisteredBufferGroup::buffer_size()`
(`registered_buffers.rs:366`) returns the post-align size and the
slot's `buffer_size()` accessor (`registered_buffers.rs:202`)
mirrors it. Two consequences:

- The 16 KiB `for_small_files` preset is rounded up to
  `next_multiple_of(4096) = 16384` on a 4 KiB-page system: a no-op.
  On a 16 KiB-page system (e.g., aarch64 Apple Silicon, some Linux
  configurations) it is also a no-op. The audit was unable to
  reproduce a case where the preset triggers a non-trivial round-up
  for the shipped values; an arbitrary user-supplied `buffer_size`
  of, say, 9 KiB on a 4 KiB-page system would be rounded up to
  12 KiB.
- The total pinned memory is computed against the post-align size,
  not the configured size. The "128 KiB" footprint cited for
  `for_small_files` in PR #3754 is the configured-size view; the
  actually-pinned size is `8 x next_multiple_of(16384, page_size)`,
  which equals 128 KiB on a 4 KiB-page system but rises to 512 KiB
  on a 16 KiB-page system. This was not called out in #3754; recording
  it here for completeness.

### 3d. Kernel slot table

The kernel maintains an iovec slot table per ring at registration
time. Slot indices are `u16`, capped at 1024 in oc-rsync via
`MAX_REGISTERED_BUFFERS`
(`crates/fast_io/src/io_uring/registered_buffers.rs:80`). The kernel
allows higher values but rejects them with `EINVAL` on most builds;
the 1024 ceiling matches the historical limit and is a safe upper
bound. A second observation that #3754 did not record: the slot
table is not free at the kernel either - each slot consumes a small
fixed amount of kernel heap for accounting structures. The 1024
ceiling is therefore both a kernel-rejection guard and an implicit
budget on per-ring memory; raising it would require a kernel-version
probe.

## 4. Performance benefit: skip per-SQE page pinning

The mechanical difference between unfixed and fixed opcodes:

| Path | Per-SQE work in the kernel |
|---|---|
| `IORING_OP_READ` / `IORING_OP_WRITE` | `get_user_pages_fast` walks the caller's address space, locates each page, increments its refcount, builds the bio with the resolved physical pages, then drops the refcounts on completion. |
| `IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` | The SQE's `buf_index` is dereferenced into the pre-pinned slot table; the kernel skips `get_user_pages_fast` entirely and goes straight to bio construction. |

Two CPU-time savings on the hot path:

1. **No `get_user_pages_fast` per SQE.** This is the headline win.
   `get_user_pages_fast` is an MMU walk plus refcount manipulation
   per page; the cost scales with `buffer_size / page_size`. For the
   default 64 KiB buffer on a 4 KiB-page system that is 16 page
   look-ups per SQE; for the large-files 256 KiB buffer it is 64.
2. **No per-completion refcount drop.** The pages stay pinned, so
   the bio teardown path skips the matching `put_user_pages_fast`.

The benefit grows linearly with SQE rate and per-SQE buffer size.
oc-rsync's `submit_read_fixed_batch`
(`crates/fast_io/src/io_uring/registered_buffers.rs:498-610`) and
`submit_write_fixed_batch`
(`crates/fast_io/src/io_uring/registered_buffers.rs:617-701`) issue
up to `min(slots, sq_entries)` SQEs per `submit_and_wait` call, so a
default-config writer flushing 64 SQEs per syscall avoids
`64 * 16 = 1024` `get_user_pages_fast` invocations per submission
batch. The savings are most visible on workloads where the kernel-
side I/O cost is small relative to MMU walking - many small SQEs,
fast NVMe, recent CPUs with large TLBs.

The cost not paid by fixed buffers, conversely, is one
`memcpy` per write: `submit_write_fixed_batch` copies the caller's
data into the slot before submission
(`crates/fast_io/src/io_uring/registered_buffers.rs:638-650`,
`memcpy` via `ptr::copy_nonoverlapping`), and reads must be copied
out of the slot into the caller's `Vec<u8>` after completion
(`registered_buffers.rs:573-588`). On large transfers this `memcpy`
overhead competes with the `get_user_pages_fast` saving; on small
transfers the saving dominates. There is no measurement loop in
oc-rsync today that picks which side of the trade-off the current
workload sits on; the design lives in
[`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md).

A second performance dimension is **concurrency**: the buffer pool
caps at `count` slots, so beyond `count` outstanding fixed SQEs the
caller falls back to the unfixed opcode (Section 5d). The default
`count = 8` is conservative; it was chosen against the observed
distribution of in-flight reads on the disk-batch path, where eight
parallel SQEs already saturated commodity NVMe.

## 5. Edge cases and the failure surface

This is the section PR #3754 covered most thinly. Each edge case
below is a real `IORING_REGISTER_BUFFERS` failure mode that the code
must tolerate.

### 5a. Low `RLIMIT_MEMLOCK`

Symptom: `register_buffers` returns `ENOMEM` because pinning
`count * aligned_size` bytes would exceed the user's locked-memory
limit.

Path:
`crates/fast_io/src/io_uring/registered_buffers.rs:307` returns
`Err(io::Error)`, caught at line 308; the function deallocates the
already-allocated user-side buffers (line 309-311) and returns
`io::Error::other("IORING_REGISTER_BUFFERS failed: <kernel msg>")`.
`try_new` discards the error, the owner stores `None`, and every
SQE thereafter takes the unfixed branch.

User-visible result: no error, no log message, no stat. The transfer
runs slightly slower because every SQE pays
`get_user_pages_fast`. There is no telemetry that distinguishes this
path from "registration disabled by config" today; both produce
`registered_buffers = None`. Fix sketch:

- Plumb the `try_new` outcome (succeeded / disabled / failed) into
  a `RegisteredBufferStatus` enum and surface via a new accessor on
  `IoUringWriter` / `IoUringReader`.
- Optionally, a `tracing::warn!` on the failure path with the
  underlying `io::Error` so operators can correlate with
  `dmesg` / `RLIMIT_MEMLOCK`.

### 5b. Kernel slot table exhausted

Symptom: `register_buffers` returns `EINVAL` when `count >
IORING_MAX_REG_BUFFERS` (kernel-defined, historically 1024).

Path: `RegisteredBufferGroup::new` rejects `count >
MAX_REGISTERED_BUFFERS` before issuing the syscall
(`crates/fast_io/src/io_uring/registered_buffers.rs:264-269`), so
this case is caught locally. Any kernel that uses a smaller cap
would surface `EINVAL` from `register_buffers` itself; the same
fall-back path as 5a applies. The cap is hard-coded; raising it
would require a kernel-version probe and is not currently planned.

### 5c. Seccomp denial

Symptom: a sandbox restricts `io_uring_register` to the
non-buffer-related opcodes only. `register_buffers` returns
`EPERM` or `ENOSYS` depending on the seccomp action.

Path: identical to 5a. The owner stores `None`, and every SQE
falls back. The unfixed path requires only `io_uring_setup`,
`io_uring_enter`, and the SQE opcodes themselves; no further
`io_uring_register` calls happen.

### 5d. Slot exhaustion at runtime

Symptom: `count` slots are checked out; a caller invokes
`checkout()` and gets `None`.

Path: `submit_read_fixed_batch` and `submit_write_fixed_batch` are
guarded by an `available() > 0` precheck inside the call sites
(`crates/fast_io/src/io_uring/file_reader.rs:158-184`,
`crates/fast_io/src/io_uring/file_writer.rs:215-246`,
`282-309`). When `available() == 0` the call site falls through to
`submit_write_batch` (the unfixed batched submitter at
`crates/fast_io/src/io_uring/batching.rs:53-152`) for that round.
The miss is recorded in `RegisteredBufferStats::total_misses`
(`crates/fast_io/src/io_uring/registered_buffers.rs:118-121,388`).
This is the only failure mode that is observable through the
existing telemetry.

### 5e. Allocation failure before the syscall

Symptom: `std::alloc::alloc_zeroed` returns null for one of the
`count` page-aligned buffers (typically `ENOMEM` from the
allocator).

Path:
`crates/fast_io/src/io_uring/registered_buffers.rs:284-301`. The
already-allocated buffers are freed
(`registered_buffers.rs:288-290`) and an
`io::ErrorKind::OutOfMemory` is returned. `try_new` swallows it.
Note that this failure happens **before** the `register_buffers`
syscall, so the kernel sees nothing.

### 5f. Drop ordering corner case

Symptom: a user constructs a `RegisteredBufferGroup` outside of one
of the documented owners and drops the group while the ring is
still alive.

Path: the test
`drop_group_before_ring_does_not_panic` at
`crates/fast_io/src/io_uring/registered_buffers.rs:1043-1070`
verifies that this is sound: the user-side pages are freed by
`Drop::drop` and the kernel still owns the pinning. The pinning is
released when the ring fd later closes. No leak, but the kernel's
view of the slot table is stale until the ring closes; any
`READ_FIXED` / `WRITE_FIXED` SQE submitted against a freed slot
would corrupt memory. There is no public API path that allows this
- every owner that calls `try_new` retains the group as long as the
ring lives.

### 5g. Explicit `unregister` errors

Symptom: a caller invokes `RegisteredBufferGroup::unregister(&ring)`
and the kernel returns `EINVAL` (e.g., the buffers were already
unregistered, or the ring was reused).

Path:
`crates/fast_io/src/io_uring/registered_buffers.rs:448-450`
returns the kernel error verbatim. The test
`unregister_after_ring_closed_returns_error_or_ok` at
`registered_buffers.rs:1159-1182` asserts the call must not panic.
Production code does not call `unregister` outside the test tree;
all production owners rely on the implicit cleanup at ring fd
close.

## 6. Comparison with PR #3754 and remaining gaps

PR #3754 (`docs/audits/io-uring-fixed-buffer-audit.md`) covered:

- Map of all `IORING_REGISTER_BUFFERS` and `READ_FIXED` /
  `WRITE_FIXED` call sites.
- The five-phase lifecycle (allocate, register, checkout, return,
  deregister).
- Pool sizing presets and the 1024 slot cap.
- Three deliberate gaps where fixed buffers are not used today
  (`SharedRing` registers without consuming, `IoUringDiskBatch` has
  no group, sockets are unfixed).
- The disjoint relationship to `PBUF_RING` / bgid namespaces.

Items this drill-down adds or sharpens:

1. **Failure-surface taxonomy.** PR #3754 said "falls back on
   registration failure" without enumerating the failure modes.
   Section 5 here lists seven distinct paths (low `RLIMIT_MEMLOCK`,
   slot exhaustion, seccomp, runtime miss, allocation, drop
   ordering, explicit unregister error) and ties each to a
   line-numbered handler.
2. **Pinned-memory accounting.** Section 3 makes the
   `RLIMIT_MEMLOCK` connection explicit. PR #3754 cited the slot
   cap but not the user-locked-memory ceiling, which is the more
   common production constraint.
3. **`with_ring` ignores `register_buffers` flag.** PR #3754 noted
   the hard-coded `count = 8`; this audit additionally notes that
   `with_ring` does not honour the `IoUringConfig::register_buffers`
   flag at all, so a caller who explicitly disabled registration
   still pays the allocation cost when the writer is built via
   `writer_from_file`. Same one-line fix in the
   `writer_from_file_with_depth` parameter list.
4. **Page-alignment footprint on 16 KiB-page systems.** Section 3c
   notes that the published "128 KiB" footprint for the
   `for_small_files` preset assumes a 4 KiB-page system; on
   16 KiB-page systems the actually-pinned size rises to 512 KiB.
5. **Telemetry blind spot.** `RegisteredBufferStats` records
   runtime checkout misses (5d) but cannot distinguish "registration
   disabled by config" from "registration failed in the kernel". A
   new `RegisteredBufferStatus` enum surfaced on each owner would
   close this gap. Operationally, this matters because a
   `tracing::info!` at owner construction is the cheapest signal an
   operator has that `RLIMIT_MEMLOCK` is too low.
6. **No pre-flight `RLIMIT_MEMLOCK` probe.** The codebase does not
   call `getrlimit(RLIMIT_MEMLOCK)` anywhere, so the only way to
   discover that registration would fail is to attempt it. A probe
   in `IoUringConfig::build_ring` could short-circuit the
   allocation when the requested footprint clearly exceeds the
   user's locked-memory budget. This is purely an optimisation;
   correctness is unaffected.
7. **At-ring-init exclusivity.** PR #3754 implied registration
   happens "speculatively" but did not foreclose the per-transfer
   or lazy models. Section 2 here records that the at-ring-init
   model is the only one used in production, that there is no
   re-registration, no resize path, and no idle deregistration, and
   that this is intentional given how rings are scoped per file
   already.

## 7. Recommendation summary

1. **Status enum** - add `RegisteredBufferStatus { Registered,
   DisabledByConfig, RegistrationFailed(io::Error) }` and surface it
   on `IoUringReader`, `IoUringWriter`, and `SharedRing`. Wire a
   single `tracing::info!` (or `tracing::debug!`) at construction
   time so the failure mode is visible without a syscall trace.
2. **Plumb `register_buffers` through `with_ring`** - one-line
   change in `writer_from_file_with_depth`; eliminates the wart
   that disabling registration via config does not affect the
   `writer_from_file` entry point.
3. **Optional `RLIMIT_MEMLOCK` probe** - read `getrlimit` once in
   `IoUringConfig::build_ring`; if the configured footprint
   (`count * next_multiple_of(buffer_size, page_size)`) exceeds the
   limit, skip `try_new` and record
   `RegisteredBufferStatus::DisabledByConfig` with a reason. Saves
   one allocation cycle per owner under low memlock.
4. **Document the 16 KiB-page system multiplier** - update the
   sizing table in PR #3754 to flag that the post-align footprint
   on 16 KiB-page systems is 4 x the configured-size figure for the
   `for_small_files` preset.
5. **Do not add `IoUringDiskBatch` registration in this PR** -
   that is Section 5b in PR #3754 and a separate optimisation.
   This audit is documentation only.

## 8. References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  has no io_uring path; this audit covers an oc-rsync local
  optimisation with no wire-protocol implication.
- oc-rsync code:
  - `crates/fast_io/src/io_uring/registered_buffers.rs:251-345` -
    `RegisteredBufferGroup::new` (only registration call site).
  - `crates/fast_io/src/io_uring/registered_buffers.rs:307` -
    the `register_buffers` syscall wrapper.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:271-272,479-487` -
    page-alignment rounding.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:80` -
    `MAX_REGISTERED_BUFFERS = 1024`.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:118-163,388` -
    `RegisteredBufferStats` telemetry.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:443-450` -
    explicit `unregister`.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:453-476` -
    `Drop` (user-side dealloc only).
  - `crates/fast_io/src/io_uring/file_reader.rs:73-81` -
    reader registration.
  - `crates/fast_io/src/io_uring/file_writer.rs:56-64,83-91,118,143-151` -
    writer registration sites (note `with_ring` at line 118
    ignores the config flag).
  - `crates/fast_io/src/io_uring/shared_ring.rs:267-275,320-322` -
    shared-ring registration without consumption.
  - `crates/fast_io/src/io_uring/config.rs:315-373` -
    `register_buffers`, `registered_buffer_count`, presets.
  - `crates/fast_io/src/io_uring/batching.rs:53-152` -
    `submit_write_batch` (unfixed fallback).
- Sibling audits:
  - [`docs/audits/io-uring-fixed-buffer-audit.md`](io-uring-fixed-buffer-audit.md)
    - PR #3754 surface map.
  - [`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md)
    - resize design.
  - [`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md)
    - disjoint PBUF_RING namespace.
  - [`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md)
    - kernel 5.19+ provided-buffer ring.
  - [`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md)
    - disk-batch path that does not yet use registered buffers.
- Linux man pages and kernel sources (verify against running kernel
  before citing in code):
  - `man 2 io_uring_register` - documents
    `IORING_REGISTER_BUFFERS`, the `iovec` payload, and the slot cap.
  - `man 2 getrlimit` - `RLIMIT_MEMLOCK` semantics.
  - `io_uring/io_uring.c::io_sqe_buffers_register` - kernel-side
    register handler that calls `pin_user_pages_fast`.
  - `io_uring/io_uring.c::io_sqe_buffers_unregister` - kernel-side
    unregister handler invoked at ring fd close.
