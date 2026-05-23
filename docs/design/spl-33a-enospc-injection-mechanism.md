# ENOSPC injection mechanism for spill fault-injection tests (SPL-33.a)

Tracking task: SPL-33.a. Companion follow-ups: SPL-33.b (implement the
chosen mechanism and the unit-test layer), SPL-33.c (assert typed-error
degradation - no panics under injection).

## Purpose

The reorder buffer spill layer
(`crates/engine/src/concurrent_delta/spill/`) writes short-lived
tempfiles whenever the in-memory ring exceeds its byte budget. The
SPL-32 audit (`docs/design/spill-fs-error-audit.md`) classified 23
filesystem syscall sites in this module - 9 recoverable, 17 bubble-up,
3 deliberately silent - and named ENOSPC as a separate failure mode
still uncovered by automated tests. SPL-35 / SPL-36 / SPL-37 (PR #4749)
shipped the typed `SpillError::PriorSpillsLost` variant and regression
coverage for the dir-wipe path; ENOSPC is the remaining hole.

This document compares five injection mechanisms, ranks them on
CI compatibility and implementation cost, and recommends a single
mechanism for SPL-33.b to implement. The recommendation is split: a
mock writer for the unit-test layer plus a `fallocate` filler for one
Linux integration test that exercises the real kernel path.

This is a design-only document. No production source is modified.

## Failure model recap

ENOSPC can be raised by the kernel on any of these sites (citations
from the SPL-32 audit):

- Site 1: `::tempfile::tempfile_in(dir)` - backend construction on a
  full filesystem.
- Site 7: `file.write_all(header)` - the 5-byte tag + length header.
- Site 8: `file.write_all(payload)` - per-item payload, the
  highest-volume site.
- Sites 9-10: whole-batch `write_record` - same payload-half failure
  as site 8 but multiple items per record.
- Site 2 (indirect): `SpooledTempFile` rollover, surfaces on the first
  `write_all` past the 1 MiB threshold.

The contract the SPL-32 audit pinned down:

- `spill_write_pos` only advances after `write_all` returns `Ok`, so
  partial records on disk are unreachable on the next attempt.
- `spill_index` and `batch_members` are mutated only after the write
  commits, so a failed write leaves the index reflecting only
  committed records.
- Per-item path re-inserts the in-flight item via `inner.force_insert`.
- Whole-batch path re-inserts via `restore_taken`.
- `SpillError::is_out_of_space()` returns `true` exactly when the
  underlying `io::Error` carries `ErrorKind::StorageFull`.

SPL-33's tests must demonstrate every one of these invariants under
forced ENOSPC, and SPL-33.c must show no `unwrap` / `expect` path
panics under injection.

## Candidate mechanisms

### 1. Bind-mount tmpfs with `size=N`

A small tmpfs is mounted (`mount -t tmpfs -o size=4M tmpfs $DIR`) and
the spill directory is pointed at it. Filling past the size cap
returns real kernel ENOSPC on the live `write(2)` syscall.

**Pros**

- Exercises the real kernel path including any vfs / filesystem
  filtering quirks.
- Deterministic byte boundary: once `size` is set, the failing offset
  is exactly the cap minus prior writes.
- Hits every site uniformly (sites 1, 7, 8, 9-10 all fire against the
  same backing filesystem).

**Cons**

- Requires root or an unprivileged user namespace with mount
  capabilities. GitHub Actions runners run as a non-root user without
  `CAP_SYS_ADMIN` in the default namespace.
- Test fixture has to clean up the mount on panic, which adds RAII
  scaffolding (`drop` impl that runs `umount`).
- Linux-only. macOS and Windows CI matrices cannot use this.

**CI compatibility**

- Linux musl: blocked. The CI image runs as a non-root user and
  `unshare -Urm` is not granted in GitHub-hosted runners.
- macOS: unsupported (no tmpfs).
- Windows: unsupported.

**Implementation cost**: M (mount/umount scaffolding, root gating,
graceful `#[ignore]` when not available).

### 2. `fallocate --length=...` filler on a tmpdir

A regular tmpdir is used as the spill directory. The test pre-fills
the free space with a sentinel file via `fallocate -l <free - cap>`
or via `posix_fallocate(3)`. Once free space drops below the
configured threshold, subsequent writes return ENOSPC at predictable
byte counts.

**Pros**

- No root required. Anyone who can write to `$TMPDIR` can fill it.
- Hits the real kernel `write(2)` path, same realism as #1.
- Easy to undo: drop the filler file at the end of the test.
- Cooperates with `tempfile::TempDir` cleanup.

**Cons**

- Filesystem-dependent. ext4 / xfs / btrfs honour `fallocate`; tmpfs
  ignores it (just allocates the bytes via the page cache); ZFS
  returns `EOPNOTSUPP`.
- macOS HFS+ / APFS expose `F_PREALLOCATE` via `fcntl`, not `fallocate`
  - portability shim required.
- Windows has `SetEndOfFile` + `SetFileValidData`, which requires
  `SeManageVolumePrivilege`; effectively unavailable on CI.
- Hits the whole shared filesystem, not just the spill dir. Risk of
  starving co-tenant tests of disk space if `cargo nextest run` is
  parallel.

**CI compatibility**

- Linux musl: yes, if `$TMPDIR` is on ext4 (it is on the GitHub
  Ubuntu image). Risk: nextest parallelism could collide.
- macOS: yes, via the `F_PREALLOCATE` shim, but parallel-test risk is
  higher because `/tmp` is the system disk.
- Windows: no.

**Implementation cost**: M (Linux primary path simple; the shim and
parallelism mitigation push it past S).

### 3. Mock writer returning `ErrorKind::StorageFull`

A new test-only `SpillBackend` variant or `WriteAdapter` wraps the
existing backend and returns `Err(io::Error::from(ErrorKind::StorageFull))`
after a configurable byte count or write call count. The error type
matches what the real kernel would return, so the error-handling code
paths (`is_out_of_space`, `force_insert`, `restore_taken`,
`SpillError::Io` mapping) all behave identically to the production
path.

**Pros**

- Pure userspace. Zero kernel involvement, zero capabilities required.
- Deterministic: the test author chooses the exact byte offset or
  write-call index at which to fail.
- Fast: no real disk I/O, the wrapper can short-circuit before
  touching the backend.
- Portable: identical behaviour on Linux, macOS, Windows.
- Composable with the existing `SpillBackend` enum - the production
  path stays untouched, the test path adds a third variant gated on
  `#[cfg(test)]`.

**Cons**

- Tests the error-handling code paths *as if* ENOSPC happened; does
  not exercise the kernel's `write(2)` boundary or any filesystem-
  specific quirks.
- Risk that a future kernel returns a different errno (`EDQUOT`,
  `EFBIG`) that the mock does not simulate. Mitigation: the mock
  takes the error kind as a constructor argument so the test matrix
  can vary it.
- Adds a small testing surface to `tempfile.rs` / `buffer/spill.rs`.
  Source change is mechanical (new enum variant + match arm) but it
  does grow the public-internal surface of the spill module.

**CI compatibility**

- Linux musl: yes.
- macOS: yes.
- Windows: yes.

**Implementation cost**: S. One enum variant, one constructor, one
match arm in `SpillBackend::file()`. Tests live in
`crates/engine/src/concurrent_delta/spill/buffer/tests/`.

### 4. `failpoints` crate with explicit injection points

The `fail` crate (https://crates.io/crates/fail) is added as a
`dev-dependencies` entry. `spill_item` and `write_record` are
instrumented with `fail_point!("spill::write_record::header", |_| { ... })`
hooks. Tests call `fail::cfg("spill::write_record::header",
"return(StorageFull)")` to arm the injection.

**Pros**

- Targets specific call sites with surgical precision (every audited
  site can have its own named fail point).
- Standard mechanism used by TiKV, sled, etc.; familiar to reviewers
  who have seen it elsewhere.
- Once instrumented, the same code is reusable for other fault
  injections (EBADF, EIO, partial writes).

**Cons**

- Requires production source changes: `fail_point!` macros are
  expanded as no-ops in release builds, but they still touch the
  spill source. The codebase has so far avoided this kind of
  instrumentation in hot paths.
- New crate dependency (`fail` is a healthy maintained crate but
  every new dep is policy review).
- The macro expansion is conditional on a build feature, which
  fragments the feature matrix (`--features failpoints`).
- Overkill for a single failure mode. The cost-benefit is favourable
  only when many injection points share the infrastructure; for
  ENOSPC-only we have one or two sites of interest.

**CI compatibility**

- Linux musl: yes.
- macOS: yes.
- Windows: yes.

**Implementation cost**: L. Adds a dependency, source instrumentation
across `spill/buffer/spill.rs`, a new build feature, and CI matrix
entries to keep the feature healthy.

### 5. `fuse-mt` userspace filesystem returning ENOSPC on demand

A `fuse-mt`-based FUSE filesystem is mounted at the spill directory.
The userspace daemon serves regular `write(2)` calls until a counter
is tripped, after which it returns `-ENOSPC` for the remainder of the
test.

**Pros**

- Most realistic injection: the kernel sees a real `write(2)` failure
  on a real fs.
- The userspace daemon can be reconfigured between tests without a
  remount.
- Reproduces filesystem-quirk failure modes (`EDQUOT`, write-back
  ENOSPC seen on close()) that mocks cannot.

**Cons**

- Requires FUSE kernel module and the `fusermount` setuid helper. CI
  images may not have either.
- Linux-only. macOS FUSE (`macfuse`) is a kext that does not run on
  arm64 macOS without manual approval and is unavailable in CI.
  Windows has nothing comparable.
- Brings up a userspace daemon thread or process per test - test
  fixture is heavyweight.
- Adds a non-trivial dependency (`fuse-mt` or `fuser` crate plus their
  transitive C deps).
- Race-prone fixture teardown: a panicked test can leave a stale
  mount.

**CI compatibility**

- Linux musl: blocked on `fusermount` availability and `CAP_SYS_ADMIN`
  for the unprivileged mount in user namespaces.
- macOS: blocked.
- Windows: blocked.

**Implementation cost**: L. The fixture, the userspace daemon, the
teardown plumbing, and the CI gating are all non-trivial.

## Comparison summary

| # | Mechanism | Linux musl | macOS | Windows | Realism | Cost |
|---|-----------|------------|-------|---------|---------|------|
| 1 | tmpfs `size=N` | blocked (root) | no | no | kernel | M |
| 2 | `fallocate` filler | yes | shim required | no | kernel | M |
| 3 | mock `Write::write` | yes | yes | yes | userspace | S |
| 4 | `failpoints` crate | yes | yes | yes | userspace | L |
| 5 | `fuse-mt` ENOSPC | blocked (fuse) | blocked | blocked | kernel | L |

## Recommendation

**Adopt mechanism #3 (mock writer) as the primary mechanism for the
SPL-33.b unit-test layer, and add mechanism #2 (`fallocate` filler)
as one Linux-gated integration test.**

Reasoning:

- The mock writer covers every behavioural contract pinned by the
  SPL-32 audit (sites 1, 7, 8, 9-10) on every CI tile, at S cost, with
  zero new dependencies and zero production source changes.
- The `fallocate` filler integration test, gated on
  `cfg(target_os = "linux")` and serialized via `nextest`'s
  `test-group`, gives one real-kernel data point that the mock layer
  is not skipping a kernel behaviour the production path depends on.
- Mechanism #1 is rejected because the root requirement is
  incompatible with GitHub-hosted runners.
- Mechanism #4 is rejected because the cost outweighs the benefit for
  a single failure mode; revisit if EBADF / EIO injection ever lands
  as additional tasks.
- Mechanism #5 is rejected because the FUSE prerequisites are absent
  in every CI matrix tile.

## Chosen mechanism: API surface (pseudo-code)

Add a `#[cfg(test)]` faulting backend to `spill/tempfile.rs`. The
existing `SpillBackend` enum gains a third variant; the existing
`open_backend` constructor is unchanged for production paths and gains
a sibling test-only constructor.

```rust
// crates/engine/src/concurrent_delta/spill/tempfile.rs

use std::io::{self, ErrorKind, Read, Seek, Write};

pub(super) enum SpillBackend {
    Spooled(::tempfile::SpooledTempFile),
    Directory(File),
    #[cfg(test)]
    Faulting(FaultingFile),
}

#[cfg(test)]
pub(super) struct FaultingFile {
    inner: ::tempfile::SpooledTempFile,
    plan: FaultPlan,
    bytes_written: u64,
    calls_made: u64,
}

#[cfg(test)]
pub(super) struct FaultPlan {
    /// Error kind to inject. Default: ErrorKind::StorageFull.
    pub kind: ErrorKind,
    /// Inject after this many cumulative bytes have been written.
    /// None = byte-count trigger disabled.
    pub fail_after_bytes: Option<u64>,
    /// Inject on the Nth write call (1-indexed). None = call-count
    /// trigger disabled. Both triggers may be set; first to fire wins.
    pub fail_on_write_call: Option<u64>,
    /// True = inject once and then succeed; false = inject persistently.
    pub one_shot: bool,
}

#[cfg(test)]
impl Write for FaultingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls_made += 1;
        if self.should_fault() {
            if self.plan.one_shot {
                self.plan.fail_after_bytes = None;
                self.plan.fail_on_write_call = None;
            }
            return Err(io::Error::from(self.plan.kind));
        }
        let written = self.inner.write(buf)?;
        self.bytes_written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}

#[cfg(test)]
impl Read for FaultingFile { /* delegate to inner */ }
#[cfg(test)]
impl Seek for FaultingFile { /* delegate to inner */ }

#[cfg(test)]
pub(super) fn open_faulting_backend(plan: FaultPlan) -> io::Result<SpillBackend> {
    Ok(SpillBackend::Faulting(FaultingFile {
        inner: ::tempfile::SpooledTempFile::new(1024 * 1024),
        plan,
        bytes_written: 0,
        calls_made: 0,
    }))
}
```

`SpillBackend::file()` gains the matching arm:

```rust
impl SpillBackend {
    pub(super) fn file(&mut self) -> &mut dyn ReadWriteSeek {
        match self {
            SpillBackend::Spooled(f) => f,
            SpillBackend::Directory(f) => f,
            #[cfg(test)]
            SpillBackend::Faulting(f) => f,
        }
    }
}
```

`SpillableReorderBuffer` already lazily opens its backend through
`open_backend`. SPL-33.b adds a `#[cfg(test)]` builder hook on the
buffer that injects a pre-opened `FaultingFile` instead of letting the
buffer call `open_backend` itself:

```rust
// crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs

#[cfg(test)]
impl<T: SpillCodec> SpillableReorderBuffer<T> {
    pub(super) fn install_faulting_backend(&mut self, plan: FaultPlan) {
        self.spill_file = Some(open_faulting_backend(plan).unwrap());
        // Force the lazy-open path off so the buffer uses our handle.
        self.spill_write_pos = 0;
    }
}
```

The `fallocate` integration test on Linux uses no new types - it
constructs the buffer with `with_spill_dir($dir)`, pre-fills `$dir`'s
filesystem to within `cap` bytes of full, and asserts the same
post-condition matrix as the mock tests.

```rust
// crates/engine/tests/spl_33_enospc_real_kernel.rs (integration test)

#[cfg(target_os = "linux")]
#[test]
fn spill_returns_storage_full_when_disk_is_full() {
    let dir = tempfile::tempdir().unwrap();
    let free = filesystem_free_bytes(dir.path());
    let filler = dir.path().join(".filler");
    fallocate(&filler, free.saturating_sub(8 * 1024)).unwrap();

    let mut buf = SpillableReorderBuffer::<Bytes>::with_spill_dir(
        ReorderBuffer::new(64),
        4 * 1024,        // 4 KiB byte budget
        dir.path(),
    )
    .unwrap();

    // Insert enough to exceed the budget; the spill must hit ENOSPC.
    for seq in 0..32 {
        let payload = Bytes::from(vec![0u8; 4096]);
        match buf.insert(seq, payload) {
            Err(SpillError::Io(e)) if e.kind() == ErrorKind::StorageFull => {
                // Expected after roughly 1-2 items pushed past the cap.
                assert!(buf.spill_stats().spilled_items < 2);
                drop(filler);
                return;
            }
            Ok(()) => continue,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    panic!("ENOSPC was not raised - check filler size vs FS reservation");
}
```

The integration test is gated on `cfg(target_os = "linux")` because
`fallocate(2)` semantics on macOS / Windows are too divergent to be
worth shimming for a single confirmation test; the mock-writer suite
already covers macOS and Windows behaviourally.

## Test matrix that SPL-33.b implements with the chosen mechanism

Reusing the matrix from the SPL-32 audit, every row maps to a
`FaultPlan` configuration:

| Scenario | `FaultPlan` |
|----------|-------------|
| ENOSPC on first `open_backend` (site 5) | `install_faulting_backend(FaultPlan { fail_on_write_call: Some(1), one_shot: false, .. })` |
| ENOSPC on header `write_all` (site 7) | `FaultPlan { fail_after_bytes: Some(0), one_shot: true, .. }` |
| ENOSPC on payload `write_all` per-item (site 8) | `FaultPlan { fail_after_bytes: Some(5), one_shot: true, .. }` |
| ENOSPC on payload `write_all` whole-batch (sites 9-10) | `FaultPlan { fail_after_bytes: Some(4), one_shot: true, .. }` |
| ENOSPC on `tempfile_in` (site 1) | Mock returns the error at backend construction via a sibling `open_faulting_backend_failing()` constructor |
| ENOSPC during `SpooledTempFile` rollover (site 2) | Configure `fail_after_bytes: Some(1024 * 1024 + 1)` so the first post-rollover write fails |
| ENOSPC followed by free-space restoration | `FaultPlan { one_shot: true, .. }` - second insert succeeds |

Every test additionally asserts:

- `buf.next_in_order()` returns the originally-inserted items in
  sequence order (proves in-memory backup survived).
- `SpillError::is_out_of_space()` returns `true` on the failing
  insert.
- `buf.buffered_count()` matches the pre-insert count when the
  failure happens before any item is durably written.
- `buf.spill_stats().spill_events` does **not** increment on the
  failing insert.
- No `unwrap` / `expect` panic was observed (this is the SPL-33.c
  contract).

## Cross-reference

This document is the design input that SPL-33.b implements. SPL-33.c
then runs the SPL-33.b test suite and asserts no production code path
panics under any of the seven `FaultPlan` configurations - the
typed-error degradation contract pinned by SPL-32.

When `SpillError::Io(e)` surfaces with `e.kind() == ErrorKind::StorageFull`,
`SpillError::is_out_of_space()` is the single API the receiver uses to
distinguish ENOSPC from generic disk failures; SPL-33's assertions
exercise that mapping.
