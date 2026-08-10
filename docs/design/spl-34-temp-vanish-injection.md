# Temp-vanish injection mechanism for spill fault-injection tests (SPL-34)

Tracking task: SPL-34. Companion follow-ups: SPL-34.b (implement the
chosen mechanism and the unit-test layer), SPL-34.c (assert typed-error
degradation - no panics under injection).

## Purpose

The reorder buffer spill layer
(`crates/engine/src/concurrent_delta/spill/`) holds short-lived
tempfiles whenever the in-memory ring exceeds its byte budget. The
SPL-32 audit (`docs/design/spill-fs-error-audit.md`) catalogued 23
filesystem syscall sites and named "temp-vanish" (the spill backing
file or its parent directory disappearing mid-transfer) as a separate
failure class still under-tested. SPL-35 / SPL-36 / SPL-37 (PR #4749)
shipped the typed `SpillError::PriorSpillsLost` variant and one
regression test for a single dir-wipe scenario; per-file and
mid-write vanish modes remain uncovered.

SPL-34 is the sibling of SPL-33.a
(`docs/design/spl-33a-enospc-injection-mechanism.md`). Where SPL-33.a
simulates "writes fail with `ErrorKind::StorageFull`", SPL-34
simulates "the inode or path the writer is holding ceases to exist".
This document compares the same toolset SPL-33.a evaluated, ranks each
mechanism against the five concrete vanish modes below, and recommends
a layered hybrid for SPL-34.b to implement.

This is a design-only document. No production source is modified.

## Failure modes covered

Each mode gets its own injection mechanism so the test author can
target one syscall window at a time.

### (a) Tempfile unlinked while open

Another process (`tmpwatch`, `systemd-tmpfiles`, sandbox janitor, a
careless operator) calls `unlink(2)` on the spill tempfile path. The
caller's fd remains valid - reads and writes against the inode keep
working - but a fresh `open(2)` of the path returns `ENOENT`. Linux,
macOS, and the WSL Linux subsystem all honour the same semantics;
Windows is the exception (an open file usually cannot be unlinked
without `FILE_SHARE_DELETE`), so this mode is Unix-primary.

Production sites at risk (per SPL-32 audit):

- Site 5 (`spill.rs:288`) - lazy `open_backend` on first spill after
  the unlink wins the race.
- Sites 7-8 / 9-10 - writes after the unlink keep working against the
  in-process fd, but the on-disk inode is reachable only through that
  fd; a panic or process death loses the data.

### (b) Tempfile truncated or replaced

A second writer creates a different inode at the same path
(`rename(2)` over the spill file, `creat(2)` with the same name after
removing the original). Reads through the cached fd still see the
**old** inode's contents - that fd was never invalidated - but any
caller-supplied path that re-opens the spill file (currently only the
lazy `open_backend` path) gets the wrong file. Read-after-write
consistency drifts silently.

Production sites at risk:

- Site 5 - first-spill `open_backend` opens the new inode while the
  buffer's `spill_index` references offsets in the old inode.
- Sites 11-19 (read hot path) - the cached fd is immune, but the
  audit's gap notes flag this as the silent-corruption window.

### (c) Parent dir vanishes mid-write

A janitor calls `rmdir(2)` on the spill directory while a `write_all`
is in progress against a tempfile inside it. Behaviour depends on the
flavour:

- `tempfile::tempfile_in` (directory backend) - on Linux this uses
  `O_TMPFILE` when supported, so the file has no path entry; `rmdir`
  fails with `ENOTEMPTY` because the unnamed file still pins the
  directory's inode. The write itself succeeds.
- On filesystems without `O_TMPFILE` the crate falls back to
  create+immediate-unlink, which leaves the directory truly empty;
  `rmdir` succeeds, the write keeps working against the unlinked
  inode, and the next `open_backend` against the (now gone) directory
  raises `ENOENT`.

Production sites at risk:

- Site 4 (`spill.rs:314`) - `recreate_spill_dir`'s `create_dir_all`
  races with the wipe.
- Site 5 - `open_backend` retry after the wipe.

### (d) Parent dir vanishes between writes

Same as (c) but the wipe lands in the quiescent interval between two
`spill_excess` calls. The buffer has cached its fd from the prior
spill; the fd keeps working for the second spill's `write_all`, but
`recreate_spill_dir` cannot run because no error fires. This is the
silent path: items written after the wipe pin the dropped inode and
cannot survive process death even if disk space is plentiful.

Production sites at risk:

- Sites 7-8 / 9-10 - writes succeed but the directory is gone.
- The next process that needs to reload from disk (after `next_in_order`
  is called past the in-memory ring's hot zone) sees the cached fd
  still reading, so no error surfaces - the silent-loss window only
  opens on process restart, which is out of test scope.

### (e) Filesystem unmounted

The mount point holding the spill tempfile is `umount`'d (forced or
lazy) while the buffer is live. Linux returns `EIO` on subsequent
syscalls against the fd; macOS returns `EIO` or `ENXIO`; on Windows
the matching scenario is device removal (USB drive yanked), which
returns `ERROR_DEVICE_NOT_CONNECTED`. This is rare in production but
common in container teardown (`podman kill` of a daemonset).

Production sites at risk: every read/write site (5-10, 11-19).
Recovery is impossible without remount, so the test contract is
limited to "fail cleanly with a typed error and do not panic".

## Candidate mechanisms

The toolset matches SPL-33.a's. Each mechanism is re-evaluated
against modes (a) through (e).

### 1. Mock filesystem layer

Wrap the existing `SpillBackend` enum with a `#[cfg(test)]` variant
whose `Read` / `Write` / `Seek` impls inject `ErrorKind::NotFound` or
`ErrorKind::Other` (with errno-equivalent kinds) after a configurable
trigger. Mirrors SPL-33.a's `FaultingFile` adapter.

**Covers**: (a) cleanly (the cached fd starts returning `NotFound`),
(b) by swapping the inner inode handle, (c)-(d) by combining with an
fd-invalidation trigger. (e) is awkward because the mock cannot
reproduce the `EIO` cascade on every cached syscall, but the
typed-error assertion still passes.

**Pros**

- Pure userspace; portable to Linux, macOS, Windows.
- Deterministic: byte offset / call count / explicit trigger.
- No new dependencies, no production source changes beyond the
  enum variant.
- Composes with SPL-33.a's `FaultPlan` - a single `FaultPlan` type
  can carry both `StorageFull` and `NotFound` failure kinds.

**Cons**

- Tests the error-handling code paths as if the kernel returned
  `ENOENT`; does not exercise actual unlink / rmdir / rename
  semantics.
- Cannot reproduce the (b) "old fd reads old inode" subtlety -
  the mock sits above the fd, not below it.

**CI compatibility**: Linux yes, macOS yes, Windows yes.

**Implementation cost**: S. Re-use SPL-33.a's `FaultingFile` chassis
and add a `VanishMode` enum to choose the kind injected.

### 2. Real `remove_file` / `remove_dir_all` in test setup

The test fixture constructs the buffer with `with_spill_dir($DIR)`,
forces one spill to populate the directory, then calls
`std::fs::remove_file` (mode a) or `std::fs::remove_dir_all` (modes
c, d) and asserts the next `insert` / `next_in_order` returns the
typed error. SPL-37
(`tests/hardening.rs:prior_spills_lost_surfaces_typed_variant_on_dir_wipe`)
already uses this pattern for one scenario.

**Covers**: (a) by `remove_file(spill_dir.join(...))` after enumerating
the directory contents, (c) and (d) by `remove_dir_all(spill_dir)`
between insert bursts. (b) is reachable via `remove_file` + `File::create`
with the same path. (e) requires `umount`, out of scope.

**Pros**

- Exercises the real kernel `unlink` / `rmdir` syscalls.
- No source changes; uses production constructors.
- Already proven in SPL-37 - low-risk pattern extension.
- Portable to Linux and macOS verbatim. Windows needs a small shim
  because open files cannot be unlinked there without
  `FILE_SHARE_DELETE`; the shim degrades to `#[ignore]` on Windows.

**Cons**

- Cannot inject vanish mid-`write_all` (mode c with a partial-write
  race) deterministically - the wipe always lands between syscalls.
  The differential between (c) and (d) collapses without instrumented
  pacing.
- Requires the buffer to expose `spill_dir()` so the test fixture can
  identify the tempfile path; the accessor already exists
  (`lifecycle.rs:232`).

**CI compatibility**: Linux musl yes, macOS yes, Windows partial (mode
b only; gate (a) on `cfg(unix)`).

**Implementation cost**: S. One helper that enumerates tempfile entries
plus the standard library calls; matches SPL-37's pattern.

### 3. `bind-mount` + `umount` for filesystem unmount (mode e)

Mount a tmpfs at the spill directory, run the test, then `umount` it
to simulate mode (e). The cached fd then returns `EIO` on every
syscall.

**Pros**

- Only mechanism that reproduces mode (e) faithfully.
- Real kernel path with no userspace adapter.

**Cons**

- Linux-only and requires `CAP_SYS_ADMIN` or a user namespace; GitHub
  Actions runners run unprivileged and cannot `mount` without
  `unshare -Urm` privileges that are not granted.
- macOS needs `mount_devfs` plus dispensation; Windows has no
  equivalent.
- Fragile fixture teardown - a panicked test leaves a stale mount.

**CI compatibility**: blocked on every tile. Skip.

**Implementation cost**: L (and not realisable on CI anyway).

### 4. `failpoints` crate

The `fail` crate instruments named injection points
(`fail_point!("spill::write_record::header_after", |_| { ... })`) in
production source. Tests arm them via `fail::cfg("...", "return(NotFound)")`.

**Covers**: all five modes if instrumented at the right sites.

**Pros**

- Surgical: each audited site can have its own named fail point.
- Standard mechanism used by TiKV, sled.
- Re-uses the same infrastructure SPL-33.a would adopt if it picked
  failpoints, so the cost amortises if multiple failure modes land.

**Cons**

- Requires production source changes (macros expand to no-ops in
  release but still touch the spill source). SPL-33.a explicitly
  rejected this for the same reason.
- New dependency under policy review.
- Build-feature fragmentation (`--features failpoints`).
- Overkill for failure modes already reachable via mechanism #1 or #2.

**CI compatibility**: Linux yes, macOS yes, Windows yes.

**Implementation cost**: L.

### 5. `fuse-mt` userspace filesystem

A FUSE filesystem at the spill directory returns `ENOENT` on demand.

**Pros**

- Full kernel realism.

**Cons**

- FUSE absent on most CI tiles; macOS FUSE needs out-of-tree kext;
  Windows has no equivalent.
- Heavy fixture per test.

**CI compatibility**: blocked. Skip.

**Implementation cost**: L.

## Comparison summary

| # | Mechanism | (a) | (b) | (c) | (d) | (e) | Linux | macOS | Windows | Cost |
|---|-----------|-----|-----|-----|-----|-----|-------|-------|---------|------|
| 1 | Mock filesystem | yes | partial | yes | yes | partial | yes | yes | yes | S |
| 2 | Real unlink / rmdir | yes | yes | partial | yes | no | yes | yes | partial | S |
| 3 | bind-mount + umount | no | no | no | no | yes | blocked | no | no | L |
| 4 | failpoints crate | yes | yes | yes | yes | yes | yes | yes | yes | L |
| 5 | fuse-mt | yes | yes | yes | yes | yes | blocked | no | no | L |

## Recommended hybrid

- **Layer 1 (unit tests)**: mechanism #1 (mock filesystem) covering
  modes (a) and (b). Purely in-process, fast, deterministic, portable
  across every CI tile. Re-use the `FaultingFile` chassis SPL-33.b is
  building so the two test suites share infrastructure.
- **Layer 2 (integration tests)**: mechanism #2 (real `remove_file` /
  `remove_dir_all`) covering modes (a), (c), and (d). Exercises the
  real syscall paths and matches the existing SPL-37 pattern; Unix
  primary, Windows mode-b only.
- **Layer 3 (skip with explicit gap)**: mode (e) filesystem unmount.
  Mechanism #3 (bind-mount + umount) is the only faithful approach
  and is incompatible with every hosted-runner CI tile. SPL-34
  documents this as a known coverage gap; reachable manually via a
  podman recipe in the rsync-profile container if a regression is
  ever suspected.

Reasoning:

- Layers 1 + 2 together cover (a)-(d) at S cost with zero production
  source changes (the mock variant lives behind `#[cfg(test)]` exactly
  like SPL-33.a's `FaultingFile`).
- Mechanism #4 (failpoints) is rejected for the same reason SPL-33.a
  rejected it: the cost outweighs the benefit when natural injection
  reaches every site of interest.
- Mechanism #5 (fuse-mt) is rejected because the FUSE prerequisites
  are absent from every CI tile.
- Mode (e) is parked as a documented gap rather than introduced as a
  flaky platform-gated test that can only run on a hand-built Linux
  image.

## API surface for Layer 1 (pseudo-code)

The mock layer extends the existing `SpillBackend` enum the same way
SPL-33.a does, but the injected error kind becomes a configurable
field rather than a hard-coded `StorageFull`. Sharing one
`FaultingFile` chassis between SPL-33 and SPL-34 keeps the surface
narrow.

```rust
// crates/engine/src/concurrent_delta/spill/tempfile.rs

#[cfg(test)]
pub(super) enum SpillBackend {
    Spooled(::tempfile::SpooledTempFile),
    Directory(File),
    Faulting(FaultingFile),
}

#[cfg(test)]
pub(super) enum VanishMode {
    /// After N writes, unlink the temp path and start returning
    /// ErrorKind::NotFound on subsequent operations.
    UnlinkOnNthWrite(u32),
    /// After N writes, atomically swap a different file into the temp
    /// path; subsequent reads still see the original inode (cached fd)
    /// but the path now points elsewhere.
    ReplaceOnNthWrite(u32, NewInode),
    /// After N writes, rmdir the spill dir. The cached fd keeps
    /// working until the next open_backend, which fails.
    RemoveDirOnNthWrite(u32),
}

#[cfg(test)]
pub(super) struct FaultPlan {
    /// Error kind to inject. ErrorKind::StorageFull (SPL-33) or
    /// ErrorKind::NotFound (SPL-34) are the two interesting values.
    pub kind: ErrorKind,
    /// Inject after this many cumulative bytes have been written.
    pub fail_after_bytes: Option<u64>,
    /// Inject on the Nth write call (1-indexed).
    pub fail_on_write_call: Option<u64>,
    /// True = inject once and then succeed; false = inject persistently.
    pub one_shot: bool,
    /// Optional temp-vanish mode (SPL-34). When Some, the chosen mode
    /// fires its real-syscall side effect (unlink / rmdir / rename)
    /// at the same trigger point as the kind injection.
    pub vanish: Option<VanishMode>,
}

#[cfg(test)]
impl<T: SpillCodec> SpillableReorderBuffer<T> {
    /// Installs a faulting backend that injects the configured failure
    /// kind at the configured trigger. Combines SPL-33 (ENOSPC) and
    /// SPL-34 (temp-vanish) into one knob.
    pub(super) fn install_temp_vanish_plan(&mut self, plan: FaultPlan) {
        self.spill_file = Some(open_faulting_backend(plan).unwrap());
        self.spill_write_pos = 0;
    }
}
```

`SpillBackend::file()` gains the matching arm exactly as in SPL-33.a.

## Per-audit-site test matrix

Mapping the 23 sites enumerated in `docs/design/spill-fs-error-audit.md`
to SPL-33 (ENOSPC) versus SPL-34 (temp-vanish) coverage. Sites flagged
"covered by SPL-37" already have a regression test; SPL-34 extends
coverage to the remaining vanish modes.

| Audit site | Operation | SPL-33 case | SPL-34 case | New coverage needed |
|------------|-----------|-------------|-------------|---------------------|
| 1 `spill/tempfile.rs:58` | `tempfile_in(dir)` | ENOSPC at construction | (c) dir vanished pre-construction | yes - SPL-34 mode (c) |
| 2 `spill/tempfile.rs:59` | `SpooledTempFile::new` | rollover ENOSPC | (e) `$TMPDIR` unmounted | parked as gap (e) |
| 3 `spill/buffer/lifecycle.rs:69` | `create_dir_all` from `with_spill_dir` | n/a | (c) parent vanished mid-call | yes - SPL-34 mode (c) |
| 4 `spill/buffer/spill.rs:314` | `create_dir_all` in `recreate_spill_dir` | n/a | (c) parent re-wiped during recovery | yes - SPL-34 mode (c) |
| 5 `spill/buffer/spill.rs:288` | lazy `open_backend` in `write_record` | ENOSPC on backend create | (a)+(c) dir vanished before first spill | covered by SPL-37 (first-spill case); extend with (a) mode |
| 6 `spill/buffer/spill.rs:291` | `file.seek` | n/a (seek rarely returns errors here) | (a) cached fd survives unlink | no new test - documented as no-op (seek on a valid fd against an unlinked inode succeeds) |
| 7 `spill/buffer/spill.rs:292` | `write_all(header)` | ENOSPC at byte 0 | (a) unlink + retry write succeeds | yes - SPL-34 mode (a) via mock |
| 8 `spill/buffer/spill.rs:293` | `write_all(payload)` | ENOSPC after header | (a) unlink between header and payload | yes - SPL-34 mode (a) mid-record |
| 9 `spill/buffer/spill.rs:147` | whole-batch `write_record` first try | ENOSPC mid-record | (c) dir wiped during write | yes - SPL-34 mode (c) via real rmdir |
| 10 `spill/buffer/spill.rs:161` | whole-batch retry after `recreate_spill_dir` | ENOSPC on retry | (c) dir re-wiped during retry | covered by SPL-37 (double-wipe surfaces fatal); add (d) variant |
| 11 `spill/buffer/reload.rs:137` | `spill_file.as_mut().ok_or(NotFound)` | n/a | n/a (synthetic NotFound, no syscall) | no |
| 12 `spill/buffer/reload.rs:140` | `file.seek` | n/a | (a) cached fd survives unlink - assert seek succeeds | optional regression - low priority |
| 13 `spill/buffer/reload.rs:144` | `read_exact(tag)` | n/a | (a) cached fd survives unlink - assert read succeeds | yes - SPL-34 mode (a) cached-fd survival |
| 14 `spill/buffer/reload.rs:149` | `read_exact(len)` | n/a | same as 13 | folded into the test for 13 |
| 15 `spill/buffer/reload.rs:154` | `read_exact(payload)` | n/a | same as 13 | folded into the test for 13 |
| 16 `spill/buffer/reload.rs:166` | batch-reload guard | n/a | n/a | no |
| 17 `spill/buffer/reload.rs:171` | batch `file.seek` | n/a | (a) same survival assertion | folded |
| 18 `spill/buffer/reload.rs:174` | batch `read_exact(len)` | n/a | same | folded |
| 19 `spill/buffer/reload.rs:178` | batch `read_exact(payload)` | n/a | same | folded |
| 20 `spill/tempfile.rs:32` | Drop `SpooledTempFile` | n/a | n/a (RAII unlink) | no |
| 21 `spill/tempfile.rs:33` | Drop `tempfile_in` File | n/a | n/a (RAII unlink) | no |
| 22 `spill/buffer/spill.rs:313` | `self.spill_file = None` | n/a | n/a (in-process drop) | no |
| 23 `spill/rss.rs:137` | `read_to_string("/proc/self/statm")` | n/a | n/a (degrades silently) | no |

Summary: 6 new SPL-34 tests land (sites 1, 3, 4, 7, 8, 9), plus 1
cached-fd survival test (site 13 folding sites 14-19), plus 1
double-wipe `(d)` extension to SPL-37's coverage of site 10. Eight
new tests in total. Site 5's existing SPL-37 case is extended with
the per-file (a) mode rather than a fresh test.

## Assertions per test

Every SPL-34 test asserts the contract pinned by the SPL-32 audit
and the new typed-error contract proposed in the open-issues section:

1. **No panic.** The production code path must not panic on any of
   the vanish modes. `unwrap` / `expect` paths in the spill module
   stay narrow (`SpillError::PriorSpillsLost { dir: self.spill_dir
   .clone() ... }` and friends).
2. **Typed-error surface.** The error returned must match one of:
   - `SpillError::PriorSpillsLost { dir, count }` when prior spills
     were committed before the vanish.
   - `SpillError::TempVanished { path }` (new variant, see open
     issues) when the vanish hits before any prior commit and recovery
     cannot complete.
   - `SpillError::Io(e)` with `e.kind() == ErrorKind::NotFound` only
     in the legacy code paths until SPL-34.b's variant lands; afterward
     this becomes `TempVanished`.
3. **Buffer survives.** Either:
   - `buf.next_in_order()` keeps returning the originally inserted
     items in sequence order (in-memory backup survived), **or**
   - the typed error surfaces and `buf.buffered_count()` equals the
     pre-insert count (the failing item was re-inserted via
     `force_insert` / `restore_taken`).
4. **No fd leaks.** Pre-test and post-test fd counts (via
   `/proc/self/fd` on Linux, `lsof -p $$` elsewhere, `_NSGetExecutablePath`
   on macOS test fixtures) must match. Test framework: a small
   `FdGuard` helper in `tests/support/` that snapshots and asserts on
   drop.
5. **No partial-write corruption observable in surviving data.** When
   the buffer recovers (mode (a) with the cached fd still valid), the
   data drained after recovery must hash-equal the data that was
   originally inserted. Test fixture: a `BlobChecksum` companion that
   the codec computes both on insert and on drain.
6. **`dir_recreate_count` is bounded.** Per the audit, recovery runs
   exactly once. Assert `buf.spill_stats().dir_recreate_events <= 1`
   across every SPL-34 scenario.

## Cross-reference

- **SPL-32** (`docs/design/spill-fs-error-audit.md`) - the 23-site
  audit this design maps tests against.
- **SPL-33.a** (`docs/design/spl-33a-enospc-injection-mechanism.md`) -
  the parallel sibling. The `FaultingFile` chassis introduced by
  SPL-33.b is re-used by SPL-34.b (one shared `FaultPlan` carries both
  ENOSPC and temp-vanish parameters).
- **SPL-33.b** - SPL-33's implementation task. SPL-34.b is its
  equivalent for temp-vanish; both tasks land in the same testing
  module to maximise chassis re-use.
- **SPL-35 / SPL-36 / SPL-37** (PR #4749) - shipped
  `SpillError::PriorSpillsLost` and one dir-wipe regression test.
  SPL-34 extends coverage to per-file vanish and mid-write vanish
  without duplicating SPL-37's directory-vanish-before-first-spill
  case.

## Open issues

### Should `SpillError::TempVanished` be a new variant or fold into `Io`?

**Recommendation: introduce `SpillError::TempVanished { path: PathBuf }`
as a new typed variant**, mirroring the SPL-35 / SPL-37 decision to
introduce `PriorSpillsLost` instead of leaving the receiver to pattern-
match on `Io(e) if e.kind() == NotFound`.

Reasoning:

- Symmetry with `PriorSpillsLost`. Both describe "the disk side of the
  spill is gone"; one with prior commits, one without. Two typed
  variants present a coherent recovery taxonomy to the receiver:
  - `PriorSpillsLost` - data lost; abort the transfer with a strong
    diagnostic.
  - `TempVanished` - no data lost yet; retry possible if the caller
    provides a different spill dir.
- The receiver's mapping layer
  (`crates/transfer/src/.../recv/...`) already pattern-matches on
  `SpillError` variants; one more variant is a one-line addition.
- Exit-code mapping stays unchanged (both new and old paths map to
  exit 11 `FileIo`), so wire compatibility with upstream rsync is
  unaffected.

Open follow-up for SPL-34.b: thread the variant through
`recreate_spill_dir` and the lazy-open path so the bare `Io(e)` with
`NotFound` no longer surfaces. Existing call sites that match on
`Io(e) if e.kind() == NotFound` migrate to the new variant; the
`PriorSpillsLost` check precedes `TempVanished` so the
"data-lost-on-disk" diagnostic still wins when both conditions hold.

### Should temp-vanish recovery attempt re-spill to a different dir?

**Recommendation: no.** Fail fast and let the caller retry the whole
operation against a fresh `SpillableReorderBuffer` constructed with a
different `with_spill_dir`.

Reasoning:

- The receiver does not currently maintain a fallback directory list;
  introducing one is a much larger design change with no clear
  default policy (per-user tmpdir? per-mount fallback? operator-
  supplied list?).
- The audit already documents that `recreate_spill_dir` retries
  exactly once and then surfaces fatal `NotFound`; multi-directory
  fallback would multiply that retry budget and obscure the underlying
  operator failure (the original tmpdir is misconfigured or actively
  hostile).
- Fail-fast preserves the "spill is best-effort scratch space"
  contract documented on `SpillBackend`. A persistent recovery layer
  belongs in a higher-level controller, not in the buffer itself.

If a future task does add multi-directory fallback, it should land as
a separate `SpillFallbackPolicy` knob on `SpillPolicy`, not as
hidden behaviour inside the buffer.
