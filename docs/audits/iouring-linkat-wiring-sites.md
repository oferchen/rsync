# io_uring `LINKAT` wiring sites

Tracking issue: oc-rsync task #1925. Predecessor work:
[`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md)
(task #1086, surveys `IORING_OP_LINKAT` alongside `RENAMEAT` / `UNLINKAT` for
the disk-commit chain). PRs #1921 and #1923 added the `IORING_OP_LINKAT`
opcode plumbing in `fast_io` and the kernel-probe gate that detects 5.15+
kernels. This audit narrows the question: now that the opcode is available,
which oc-rsync call sites actually want it, what does the wiring look like,
and is the cost worth the benefit?

## Scope

Three concrete questions:

1. Where do we call `link(2)` / `linkat(2)` today, and which of those sites
   would benefit from io_uring submission?
2. How does the ring get threaded into those sites without disturbing the
   existing fallback / portability story?
3. What is the realistic per-workload speedup, and what is the
   correctness / complexity cost of taking it?

This is a documentation-only audit. No Rust code changes are proposed here;
all proposals route through follow-up issues.

## Upstream evidence

`target/interop/upstream-src/rsync-3.4.1/` has no io_uring usage. Hardlink
finalisation in upstream is a serial sequence of `link(2)` calls in
`hlink.c:hard_link_one()` and `hlink.c:finish_hard_link()`. The upstream
disk path uses plain `link(2)` / `linkat(2)` via `do_link()` /
`do_linkat()` in `syscall.c`. Any io_uring batching is therefore a pure
oc-rsync optimisation with no wire-protocol implication.

## TL;DR

There are three classes of `link(2)` / `linkat(2)` call site in oc-rsync,
and only **two** of them benefit from `IORING_OP_LINKAT`:

| Site | Crate path | Volume | Benefit |
|------|------------|--------|---------|
| Receiver follower hardlink finalise | `crates/transfer/src/receiver/directory/links.rs:239` | High when `-H` and many siblings finalise together | **Yes**: batch of N `linkat` calls amortises into one `io_uring_enter` |
| `--link-dest` quick-check link | `crates/transfer/src/receiver/quick_check.rs:177` | High when many up-to-date entries reuse a basis dir | **Yes**: independent across entries; safe to batch |
| `O_TMPFILE` materialise | `crates/fast_io/src/o_tmpfile/low_level.rs:208` (`libc::linkat`) | One per file commit | **Marginal**: needs `/proc/self/fd/N` resolution under io_uring; gated by per-kernel probe; chained inside the disk-commit triple this is already covered by audit #1086 |
| Local-copy hardlink replication | `crates/engine/src/local_copy/overrides.rs:54` (`fs::hard_link`) | One per replicated link in the source tree | **No** (single-link site, no batching opportunity, hot-path is the data copy not the link call) |

Recommendation: wire `IORING_OP_LINKAT` into the receiver follower-link pass
first (it has the largest batch sizes and the cleanest semantics), and into
the `--link-dest` quick-check pass second. Leave the `O_TMPFILE` site to the
disk-commit batching work tracked by audit #1086, where the linkat is the
**tail** of a `write -> fsync -> linkat` chain. Skip the local-copy site
entirely - it is not a per-batch call.

## 1. Hardlink call sites in scope

Files inspected (paths relative to repo root):

- `crates/transfer/src/receiver/directory/links.rs`
- `crates/transfer/src/receiver/quick_check.rs`
- `crates/transfer/src/receiver/file_list.rs`
- `crates/engine/src/local_copy/overrides.rs`
- `crates/engine/src/local_copy/context_impl/state.rs`
- `crates/fast_io/src/o_tmpfile/low_level.rs`
- `crates/fast_io/src/temp_file_strategy.rs`
- `crates/fast_io/src/io_uring/disk_batch.rs` (existing batching shape)
- `crates/fast_io/src/io_uring/batching.rs` (chain helpers)
- `crates/fast_io/src/kernel_version.rs` (kernel probe)
- `crates/metadata/src/` (no `link()` / `hard_link()` call sites; the
  `crates/metadata/src/stat_cache.rs` and `crates/metadata/src/symlink_munge.rs`
  matches in the initial grep are the strings `symlink` / `hard_link_check`
  in unrelated identifiers, not link syscalls)

### 1.1 Receiver follower-link pass (highest payoff)

`crates/transfer/src/receiver/directory/links.rs:239` is the
`fs::hard_link(&leader_path, &link_path)` call inside
`Receiver::create_hardlinks`. Upstream parity: `hlink.c:maybe_hard_link()`
-> `atomic_create()` -> `do_link()`.

Shape of the surrounding loop (`crates/transfer/src/receiver/directory/links.rs:182-257`):

```
for entry in &self.file_list:
    if not (hlinked && !hlink_first): continue
    leader_path = tracker.leader_path(leader_idx)
    link_path = dest_dir.join(relative_path)
    # Quick-check: skip if dev/ino already match
    if link_meta.dev() == leader_meta.dev() && link_meta.ino() == leader_meta.ino():
        emit_itemize(); continue
    fs::remove_file(&link_path)         # if dest exists
    fs::create_dir_all(parent)          # ensure parent
    fs::hard_link(&leader_path, &link_path)
    emit_itemize(...)
```

Key properties for batching:

- The set of follower entries is known up-front from `self.file_list` once
  the leader transfer has settled.
- Each follower's link is **independent** of every other follower's link
  (different `link_path`, leader_path can differ across groups).
- The `remove_file(&link_path)` precondition for `linkat`'s `EEXIST` is
  itself a candidate for `IORING_OP_UNLINKAT` on the same chain, but the
  audit recommends keeping the `unlink` outside the io_uring batch in v1
  - it has more failure modes (the destination might be a directory, a
  symlink, fall through to backup, etc.) and the batch becomes much harder
  to reason about.
- The itemize event is emitted **after** the link succeeds; reordering /
  multishot completion fits naturally into a "drain CQEs in submission
  order, emit itemize for each success" pattern.

Workload sensitivity: `-H` (preserve hardlinks) on a tree with M leaders
each having K followers issues `M * (K - 1)` calls during the
`create_hardlinks` final pass. For real workloads (kernel source tree:
~38k hardlinks; node_modules: ~12k; backup images: 100k+) the batch is
several thousand calls in tight succession. This is the largest single
hardlink-only batch in oc-rsync.

### 1.2 `--link-dest` quick-check pass (medium payoff)

`crates/transfer/src/receiver/quick_check.rs:177` is the
`fs::hard_link(&ref_path, &dest_path)` call inside `try_reference_dest`.
Upstream parity: `generator.c:991 hard_link_one()` for match_level 3 with
`LINK_DEST`.

Shape:

```
for ref_dir in reference_directories:
    ref_path = ref_dir.path.join(relative_path)
    if !ref_meta.is_file(): continue
    if !quick_check_matches(...): continue
    if ref_dir.kind == Link:
        fs::create_dir_all(parent)
        if fs::hard_link(&ref_path, &dest_path).is_ok():
            apply_metadata_from_file_entry(&dest_path, entry, ...)
            apply_acls_from_receiver_cache(&dest_path, entry, ...)
```

Properties:

- This is called once per file entry that the generator decides to
  consider for `--link-dest` reuse. Volume scales with the number of
  files **in the basis tree that are also unchanged**, which is exactly
  the fast path for backup workloads (`rsnapshot`, `rsync.net`-style).
- The post-link metadata + ACL apply step adds two more syscalls per
  successful link, which dominate the per-entry cost. io_uring batching
  the `linkat` alone reduces 1 syscall per file; batching the metadata
  apply is out of scope (and audit #1086 already discusses ACL/utimens
  via io_uring).
- Crucially, the entry is only batched **after** the quick-check has
  matched. The batch is built incrementally by the generator-driven
  loop, not from a pre-staged list.

Volume: for a daily backup of an unchanged 100k-file tree this loop
issues ~100k `linkat` calls. This is the workload that shows the
strongest absolute speedup from batching, but only after the per-entry
metadata apply path is also amortised.

### 1.3 `O_TMPFILE` materialise (covered by audit #1086)

`crates/fast_io/src/o_tmpfile/low_level.rs:208` calls `libc::linkat`
directly (with `AT_SYMLINK_FOLLOW` and `/proc/self/fd/N` source) to
materialise an `O_TMPFILE` anonymous inode at its final destination. This
is one `linkat` per committed file, and it is the **tail** of a
write -> fsync -> linkat sequence.

Audit
[`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md)
covers the full chain (`IOSQE_IO_LINK` linking `IORING_OP_WRITE` ->
`IORING_OP_FSYNC` -> `IORING_OP_LINKAT`). Wiring `LINKAT` here in
isolation, outside that chain, would not unlock new amortisation: the
write and fsync remain serial. Recommendation: do not duplicate the
linkat wiring inside `o_tmpfile/low_level.rs`; let it become a step in
the eventual disk-commit chain helper, with the `libc::linkat` fallback
preserved for non-batch callers.

Special concern: the `/proc/self/fd/N` source path. The kernel resolves
this differently under io_uring than under a normal syscall; `man 2
io_uring_enter` does not yet document the procfs symlink behaviour for
`IORING_OP_LINKAT`, and PR #1923 added a probe (per audit #1086 section
2.1) that confirms the opcode is present but does not validate the
procfs-resolution path. Until that probe is extended to cover the
`AT_SYMLINK_FOLLOW` + `/proc/self/fd/N` case, the wiring should keep
the libc fallback.

### 1.4 Local-copy hardlink replication (out of scope)

`crates/engine/src/local_copy/overrides.rs:54` is `fs::hard_link(source,
destination)` inside the local-copy executor, replicating a single
hardlink relationship from source to destination. This is one call per
detected hardlink in the source tree, interleaved with the data-copy
loop. There is no batch surface to amortise across, and the data copy
itself dominates the per-file cost. **Skip.**

### 1.5 Test fixtures

The remaining `fs::hard_link` matches in the grep are test fixtures
(`crates/engine/src/local_copy/tests/execute_hardlinks.rs`,
`crates/daemon/src/tests/chunks/daemon_hardlinks_relative_receive.rs`,
etc.). These produce tree state for tests; they are not on the
production path.

## 2. PR #1921 / #1923 surface

PR #1921 added the `IORING_OP_LINKAT` opcode wiring in `fast_io` (the
`io-uring = "0.7"` crate already exposes `opcode::LinkAt`; the PR added
the oc-rsync side helper that constructs the SQE with `Path` arguments,
SQE flag plumbing, and `OpTag::LinkAt` for completion routing).

PR #1923 added the kernel probe gate in `crates/fast_io/src/kernel_version.rs`.
The probe queries `IORING_REGISTER_PROBE` for `IORING_OP_LINKAT` (op
number 37) and records availability in `IoUringKernelInfo`. Callers can
ask `IoUringKernelInfo::supports(OpTag::LinkAt)` (or the equivalent
const-detected helper) before submitting, and fall back to `libc::linkat`
or `std::fs::hard_link` if the opcode is missing.

This is the same probe shape used for `IORING_OP_FSYNC_DATASYNC` and
`IORING_OP_RENAMEAT`. The audit assumes that shape stays stable for
follow-on work.

## 3. Wiring sketch

### 3.1 Ring threading

The receiver does not currently own an `IoUringDiskBatch`. The
`IoUringDiskBatch` instance lives on the disk-commit thread
(`crates/transfer/src/disk_commit/thread.rs`); the receiver runs on a
different thread and would have to either:

- **Option A**: Construct its own ring instance scoped to
  `Receiver::create_hardlinks`. Cheap to build and tear down per
  receiver run. Avoids cross-thread submission. Ring size: 256 SQEs is
  enough for the largest follower batch we have seen.
- **Option B**: Reuse the disk-commit thread's ring by sending a
  `LinkBatch { (leader, follower)+ }` message over the SPSC pipeline.
  Avoids constructing a second ring but couples the receiver to the
  disk-commit thread's lifecycle, and the disk thread is by definition
  busy committing files at this point.
- **Option C**: Reuse a shared `IoUringSession` instance per audit
  [`docs/audits/shared-iouring-session-instance.md`](shared-iouring-session-instance.md),
  if/when that lands.

Recommendation: start with Option A. The follower-link pass is
self-contained, runs after all data transfer is finished (the disk
thread is winding down by then), and a ring built on the spot has no
ordering interaction with the disk-commit ring. Migrate to Option C
when the shared session lands.

For the `--link-dest` quick-check pass the trade-off shifts: the loop
runs **inside the generator** before the disk thread has anything to
do, so a generator-side ring is appropriate. Treat it as a separate
follow-up.

### 3.2 Submission shape

For Option A on the follower-link pass, the v1 submission is a flat
batch of independent `IORING_OP_LINKAT` SQEs:

```
let mut sq = ring.submission();
for (leader, follower) in batch {
    let entry = opcode::LinkAt::new(
        AT_FDCWD, leader.as_ptr(),
        AT_FDCWD, follower.as_ptr(),
    )
    .flags(0)            // no AT_SYMLINK_FOLLOW; both are concrete paths
    .build()
    .user_data(idx);     // index back into batch
    unsafe { sq.push(&entry)?; }
}
drop(sq);
ring.submit_and_wait(batch.len())?;
for cqe in ring.completion() {
    let res = cqe.result();
    if res < 0 { record_error(batch[cqe.user_data()], res); }
    else      { emit_itemize(batch[cqe.user_data()]); }
}
```

Why no `IOSQE_IO_LINK`: the per-link operations are independent. Failure
of one link must not cancel the others. (This is the opposite of the
disk-commit triple in audit #1086 which **does** want chaining.)

`Path` argument lifetime: the SQE captures `*const c_char`. Each leader
and follower path must live until the CQE drains. The batch vector
itself can own `CString` copies; this is the same pattern the
disk-commit ring uses for rename source/destination strings.

### 3.3 Fallback chain

```
LINKAT (io_uring SQE)         # if IoUringKernelInfo::supports(OpTag::LinkAt)
  fallback on probe-miss ->
linkat(2)  via libc           # always available on Linux 2.6.16+
  fallback on EXDEV / ENOSYS / non-Linux ->
link(2) / std::fs::hard_link  # POSIX baseline; matches std today
```

The probe is one-shot per process (`OnceLock`), so the dispatch cost is
a single atomic load per batch. If the probe says "no LINKAT", the
batch falls back to a serial `std::fs::hard_link` loop, which is
exactly today's behaviour. There is no observable behaviour change for
non-Linux hosts, for kernels < 5.15, or for `io-uring` cargo feature
disabled.

Errors must be mapped per-link (not per-batch). The CQE result is the
linkat errno negated (see `man 2 io_uring_enter`); convert with
`io::Error::from_raw_os_error(-res)` and continue draining. A single
`-EEXIST` for one follower must not abort the rest of the batch.

### 3.4 Kernel-probe interaction

PR #1923's probe records availability per opcode; the linkat wiring
should additionally validate at probe time that submitting a no-op
LINKAT (linking a sentinel path to itself in `/tmp`) does not return
`-EINVAL` on this kernel, mirroring the conservative validation done
for the splice opcode in
[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md). For the
`O_TMPFILE` site (section 1.3) the probe must additionally include
`AT_SYMLINK_FOLLOW` + `/proc/self/fd/N`; today's PR #1923 probe does
not - extending it is a prerequisite for wiring io_uring linkat into
`o_tmpfile/low_level.rs`.

## 4. Cost / benefit

### 4.1 Where the win is real

- **Receiver follower-link pass** (`-H` workloads with thousands of
  followers): batching reduces N syscall round-trips to one
  `io_uring_enter`. At ~150 ns per `linkat(2)` syscall on a modern
  x86_64 kernel, 10k followers is ~1.5 ms of pure syscall overhead;
  batching brings that to ~50 us (one enter + one drain). Disk-side
  link time is unchanged; the win is purely in user-space syscall
  overhead. For workloads where the follower-link pass is on the
  critical path (e.g. hardlink-heavy tree refresh) this is visible in
  end-to-end time.
- **`--link-dest` quick-check pass on unchanged trees**: same
  arithmetic, similar magnitude. For 100k files the saved syscall
  overhead is ~15 ms; against the per-file metadata-apply cost
  (~5-10 us) that is a 5-10% improvement on the up-to-date hot path.

### 4.2 Where the win is illusory

- **Single-file linkat sites** (`O_TMPFILE` materialise, local-copy
  replicate, daemon paths): no batch, no win. The linkat is dwarfed by
  the rest of the per-file work.
- **Small batches** (<8 entries): the ring submit/drain pair has
  fixed-cost overhead of ~2 us; the break-even point against serial
  `linkat(2)` calls is ~12 entries. Below that, do not even try.

### 4.3 Costs

- **Code**: ~150-200 lines for the batched-linkat helper plus the
  fallback path. Manageable, but new infrastructure to maintain.
- **Probe extension**: confirming `AT_SYMLINK_FOLLOW` + procfs
  resolution under io_uring is non-trivial - it requires a real-world
  smoke test the probe does not perform today. Without that, the
  `O_TMPFILE` path stays on libc.
- **Test surface**: each fallback step needs explicit coverage
  (`io_uring` feature on/off, kernel < 5.15 simulation via probe-mock,
  `EXDEV` cross-device, `ENOSPC`, `EEXIST` after the dest-cleanup
  step). The `EEXIST` case in particular is workload-specific and
  needs a deterministic test fixture.
- **Error semantics drift**: io_uring CQE results carry only the
  errno; the libc path produces an `io::Error` with optional path
  context. Extension trait must reattach paths from the batch's
  `user_data` index.
- **Observability**: add a `linkat_io_uring_used` counter to the
  receiver summary so we can confirm in production that the wiring is
  actually firing on supported hosts. Without it, a silent fallback
  (kernel says no, we go to libc) is invisible.

### 4.4 When to wire it

Recommended phasing:

1. Land the receiver follower-link pass behind the existing
   `--io-uring=auto|enabled|disabled` policy, gated on the PR #1923
   probe. Default to `auto`. Smoke-test on the rsync-profile container
   (Linux 6.x).
2. Benchmark on the canonical hardlink-heavy workload in
   `scripts/benchmark.sh` (kernel-tree-with-hardlinks fixture). Ship
   only if the end-to-end improvement is measurable (>= 3% on
   `-aH --delete` of a 100k-file tree). Otherwise keep the libc path.
3. After (2) ships and is stable, wire the `--link-dest` quick-check
   pass (separate task).
4. After audit #1086's disk-commit chain helper lands, consume
   `IORING_OP_LINKAT` as the chain tail for the `O_TMPFILE` site
   (separate task; do not duplicate the wiring).

Skip indefinitely:

- Local-copy `overrides.rs::create_hard_link` - no batch surface.
- Daemon-internal hardlink test fixtures - test code, not production.

## 5. Open questions

- Does `IORING_OP_LINKAT` correctly resolve `/proc/self/fd/N` symlinks
  under all 5.15+ kernels? Probe extension required before the
  `O_TMPFILE` site can move.
- Does the probe need a per-mount fallback for filesystems that reject
  `linkat` regardless of submission method (e.g. some FUSE mounts)?
  The libc path already handles this; the io_uring path inherits the
  same `EXDEV` / `EPERM` error code, so the fallback is automatic, but
  the test matrix should include at least one FUSE configuration.
- For Option A (per-receiver ring), does the receiver hold the ring
  open across the entire follower-link pass, or rebuild per-batch? The
  follower set is bounded by `self.file_list` size, so a single ring
  for the whole pass is correct; the question is whether the ring
  fixed-fd table conflicts with the disk-commit ring. Today it does
  not (different threads, different fd tables), but a future shared
  session (Option C) would have to coordinate.
