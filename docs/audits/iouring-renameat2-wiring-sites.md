# io_uring RENAMEAT2 wiring sites

Tracking issue: oc-rsync task #1924. Branch: `docs/iouring-renameat2-wiring-1924`.

## Scope

Identify every `rename` / `fs::rename` call in the receiver-side disk commit
thread and the local-copy executor, document the current syscall and error
handling, and judge whether wiring each site to the io_uring `RENAMEAT2`
opcode (already shipped as a fast_io primitive) would yield meaningful
throughput. The audit is restricted to the two trees the user nominated:

- `crates/transfer/src/disk_commit/`
- `crates/engine/src/local_copy/executor/`

The infrastructure that would back any wiring already lives in
`crates/fast_io/src/io_uring/renameat2.rs` and was added by PR #3739
(commit `9086f7f78`, closes issues #1920 and #1922). That PR ships:

- `renameat2_supported()` - cached `OnceLock` opcode-35 probe via
  `IORING_REGISTER_PROBE` (kernel 5.11+).
- `RenameAt2Args<'a>` - borrowed `&CStr` argument struct compatible with
  `AT_FDCWD` and arbitrary directory file descriptors.
- `build_renameat2_sqe` / `build_renameat2_sqe_unchecked` - SQE builders
  that re-export `RENAME_NOREPLACE`, `RENAME_EXCHANGE`, `RENAME_WHITEOUT`.
- `renameat2_blocking()` - one-shot transient ring helper.
- A non-Linux stub in `crates/fast_io/src/io_uring_stub.rs` so the public
  surface is identical across platforms (always `Unsupported`).

No call site in the workspace currently invokes any of these helpers; every
rename in the disk commit and local-copy paths still goes through
`std::fs::rename`, which on Unix is a `renameat(AT_FDCWD, ..., AT_FDCWD, ...)`
syscall. The question this audit answers is: should that change, and if so,
where?

## Inventory of rename sites

`grep -rn "rename\|fs::rename" crates/transfer/src/disk_commit/
crates/engine/src/local_copy/executor/` reports four rename calls; the rest
of the matches are doc-comments, identifier names, or rename-related state
fields. The four calls below are the only sites that issue a syscall.

### Site 1 - temp+rename commit on the disk commit thread

- **File**: `crates/transfer/src/disk_commit/process.rs:312`.
- **Call**: `fs::rename(cleanup_guard.path(), &begin.file_path)?;`
- **Driver**: `commit_file()` (process.rs:298), invoked from
  `process_file()` (process.rs:99) and `process_whole_file()`
  (process.rs:192) on every `FileMessage::Commit`.
- **Strategy**: traditional temp-file-and-rename. The temp path is
  produced by `temp_guard::open_tmpfile` and held by `TempFileGuard`
  (RAII); after a successful rename `cleanup_guard.keep()` suppresses
  the guard's drop-time `unlink`.
- **Current syscall**: `renameat(AT_FDCWD, temp, AT_FDCWD, dest)` via
  `std::fs::rename`. No flags, no atomic-swap semantics. Errors are
  bubbled up through `io::Result<()>` with a single `?` - no retry, no
  fallback. `EXDEV` propagates; there is no cross-device copy.
- **Upstream parity**: matches `receiver.c:recv_files()` ->
  `util1.c:robust_rename()` semantically, but oc-rsync's disk-commit
  rename does **not** retry `ETXTBSY`. That retry exists only at site 4.
- **Frequency**: one per committed file. This is the dominant rename site
  for receiver-side transfers.

### Site 2 - backup rename before overwrite

- **File**: `crates/transfer/src/disk_commit/process.rs:431`.
- **Call**: `fs::rename(file_path, &backup_path)`
- **Driver**: `make_backup()` (process.rs:412), invoked from
  `commit_file()` only when `config.backup` is `Some`.
- **Strategy**: in-place rename of the **destination** file to its backup
  path before `commit_file` does the temp->dest rename. Mirrors upstream
  `backup.c:make_backup()`.
- **Current syscall**: same `renameat(AT_FDCWD, ...)`. Returns
  `io::Result<()>` directly. `make_backup` short-circuits with
  `Ok(())` when the destination does not exist (`file_path.exists()`
  pre-check at process.rs:413, which costs a `stat`).
- **Frequency**: one per committed file when `--backup` / `--backup-dir`
  is active. Off by default.

### Site 3 - local-copy executor: success path commit

- **File**: `crates/engine/src/local_copy/executor/file/guard.rs:308`.
- **Call**: `fs::rename(&temp_path, &self.final_path)`
- **Driver**: `DestinationWriteGuard::commit_named_temp_file()` (guard.rs:305),
  invoked from `commit()` (guard.rs:271). Used by every local-copy
  finalize that takes the named-temp branch (the anonymous `O_TMPFILE`
  branch goes through `linkat`, not `rename`).
- **Strategy**: temp+rename with explicit retry. The first attempt is
  the "happy path".
- **Current syscall**: `renameat(AT_FDCWD, ...)`. On `Ok`, returns.
  On error, branches by `io::ErrorKind`; see site 4 for the retry.
- **Frequency**: one per local-copy finalize that does not use
  `O_TMPFILE`. Heavy on macOS, BSD, and Linux kernels predating 3.11.

### Site 4 - local-copy executor: retry-after-clobber

- **File**: `crates/engine/src/local_copy/executor/file/guard.rs:312`.
- **Call**: `fs::rename(&temp_path, &self.final_path)`
- **Driver**: same as site 3, but inside the
  `io::ErrorKind::AlreadyExists` arm of `commit_named_temp_file`.
  Mirrors upstream `util1.c:robust_rename()` which retries up to four
  times on `ETXTBSY` after `unlink(dest)`. The cross-device fallback
  (`ErrorKind::CrossesDevices` at guard.rs:324) does **not** call
  rename; it falls back to `fs::copy` + `fs::remove_file`.
- **Current syscall**: `renameat(AT_FDCWD, ...)`. On a second failure,
  the error is wrapped in `LocalCopyError::io` with the
  `finalise_action()` label.
- **Frequency**: only when the destination already existed at first
  rename or the executable was busy; rare on steady-state transfers.

## What RENAMEAT2 would buy at each site

The kernel-side savings of `IORING_OP_RENAMEAT` versus a synchronous
`renameat`/`renameat2` syscall are:

1. Submission can be **batched**: an SQE can be queued without crossing
   the syscall boundary, then drained for many files in one `io_uring_enter`.
2. Submission can be **chained** (`IOSQE_IO_LINK`) with the preceding
   `IORING_OP_FSYNC` and following metadata SQEs, removing two userspace
   round-trips per file.
3. The `flags` field exposes `RENAME_NOREPLACE` and `RENAME_EXCHANGE`,
   primitives that `std::fs::rename` cannot express. `RENAME_NOREPLACE`
   would make `--ignore-existing` enforceable at the kernel level instead
   of via a TOCTOU stat; `RENAME_EXCHANGE` would let `make_backup` swap
   live + backup atomically instead of doing rename-then-rename.

What it does **not** buy: a single rename in isolation is already cheap.
A `renameat(2)` on tmpfs is on the order of 1-2us; on ext4/xfs with a
short pathname it is 3-5us; on btrfs/zfs subvolumes 10-20us. The crossing
into io_uring submission, completion reaping, and CQE demux costs roughly
the same per-op. Net gain only appears when (a) many renames are issued
back-to-back so they share a `submit_and_wait`, or (b) the rename is
chained with an op that **does** benefit from io_uring (write or fsync).

## Cost / benefit per site

| Site | Hot? | Chains with fsync? | Wire-it Verdict |
|------|------|--------------------|-----------------|
| 1 - disk_commit `commit_file` | Yes (one per committed file) | Yes (`flush_and_sync` on the line above) | **Yes**, but only as the last link of an existing IOSQE_IO_LINK chain. Standalone wiring breaks even at best. |
| 2 - disk_commit `make_backup` | No (off by default) | No | No. The `file_path.exists()` pre-check is a `stat`, not a `rename`; the gain would be one syscall on an opt-in path. Not worth the borrowed-`CStr` plumbing. |
| 3 - local_copy `commit_named_temp_file` happy path | Yes for local copies | No (fsync is not driven from this path on most platforms) | Defer. Local copies already benefit far more from `O_TMPFILE` + `linkat` (already wired via the `Anonymous` branch). Wiring RENAMEAT2 here without a chain is a wash. |
| 4 - local_copy retry-after-clobber | Cold | No | No. Error-path latency is irrelevant. |

## Hot sites worth wiring

Only **site 1** clears the bar, and only when wired as the tail of a
chain that already has the full pipeline running through io_uring. The
preconditions for the chain to pay off are:

1. The disk-commit thread already submitted the file's writes through
   `IoUringDiskBatch` (this happens today when `disk_batch.is_some()`
   and sparse mode is off; see `process.rs:280-285` for the gate).
2. `--fsync` is active so an `IORING_OP_FSYNC` is in the chain. Without
   fsync, the only operation queued is the rename, and the chain
   degenerates to a single SQE - no batching gain.
3. The kernel is 5.11+ (RENAMEAT) **and** 5.15+ if we also want
   `IORING_OP_LINKAT` for the `O_TMPFILE` commit path on the
   local-copy side. The probe in `renameat2_supported()` already
   covers the 5.11 floor.

The expected win from wiring site 1 under those preconditions is one
syscall removed per committed file: `submit_and_wait` already crosses
the boundary for the writes and the fsync, so the rename rides for
free. For receiver-side transfers of N small files with `--fsync`, the
saving is N `renameat` syscalls, or roughly 1-5us * N. On a million-file
transfer that is 1-5 seconds of wall time, but the **same** files are
already paying tens of seconds in fsync, so the relative win is in the
1-3% range on the disk-commit thread.

For transfers without `--fsync`, the chain only has (writes, rename).
The rename is cheap relative to the writes themselves, and the writes
already batch across files; appending RENAMEAT2 at the end of the
batch saves one syscall per **file**, but the SQE queue depth and the
need to keep the temp+final `CStr` storage alive through the CQE drain
cost more in code complexity than the syscall gain.

## Recommendation

1. **Site 1 (Yes, conditional)**: Wire `IORING_OP_RENAMEAT` as the tail
   SQE of the existing disk-commit chain only when both `disk_batch` is
   active and `do_fsync` is true. The chain becomes
   `IORING_OP_WRITE`* -> `IOSQE_IO_LINK` -> `IORING_OP_FSYNC` ->
   `IOSQE_IO_LINK` -> `IORING_OP_RENAMEAT`. Borrow `&CStr` storage from
   `BeginMessage::file_path` and `TempFileGuard::path()` for the
   duration of the submission; both already outlive `commit_file`.
   `cleanup_guard.keep()` must be called after the CQE returns success,
   not before submission.
2. **Sites 2, 3, 4 (No)**: Leave on `std::fs::rename`. No measurable win.
3. **Future opt-in**: `RENAME_NOREPLACE` is the only RENAMEAT2 flag that
   could justify wiring **independent** of the chain. It would let
   `--ignore-existing` enforce its invariant atomically at the kernel
   level instead of via the current
   `dest.exists()` -> rename TOCTOU pattern. That is a separate task
   (`--ignore-existing` lives in the generator, not the disk-commit
   thread) and is out of scope for #1924.

## Estimated wall-clock impact

Conservative bound for a receiver-side bulk transfer (1M files, 4KiB
median, `--fsync`, ext4, NVMe):

- Without wiring: ~1.5s of total `renameat` syscall time (1.5us median).
- With wiring (site 1 only, chained behind fsync): ~0.0s of `renameat`
  cost; one CQE reap added per file (~200ns), so net saving ~1.3s.
- Disk-commit thread total today: ~80-120s for the workload above
  (dominated by fsync). Relative win: 1-2%.

The win scales linearly with file count and inversely with file size.
For workloads dominated by very small files (e.g. node_modules-style
trees) it grows to 3-5% of disk-commit thread time. For workloads with
median file sizes above 64KiB the wiring is invisible.

## Out of scope

- Generator-side rename calls (delete-after, partial-dir promotion).
- Daemon-side rename (no rename calls in the daemon path).
- `linkat`-based commit on the `O_TMPFILE` branch in
  `engine/src/local_copy/executor/file/guard.rs:358` - already wired
  through `fast_io::link_anonymous_tmpfile`, which is the analogue of
  this audit for opcode 37 (`IORING_OP_LINKAT`, 5.15+).
- `RENAME_EXCHANGE` use for `--backup` swap. Possible follow-up; the
  semantics differ from upstream's rename-then-rename and would need
  an interop carve-out.
