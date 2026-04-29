# io_uring batching for the disk commit thread

Tracking issue: oc-rsync task #1086. Branch: `docs/iouring-disk-commit-audit`.

## Scope

Evaluate whether the dedicated disk commit thread should batch its per-file
commit syscalls (write -> fsync -> rename / linkat) via io_uring linked SQE
chains (`IOSQE_IO_LINK`) instead of issuing them serially through the standard
`std::fs` and libc paths. The audit covers what code currently runs on the
disk thread, what io_uring infrastructure already exists in `fast_io`, what a
batched commit would look like at the kernel level, and the risks specific to
oc-rsync's commit semantics (`--inplace`, `--partial-dir`, `--fsync`,
`O_TMPFILE` + `linkat`, `--backup`).

Source files inspected (all paths repository-relative):

- `crates/transfer/src/disk_commit/mod.rs` (module shape).
- `crates/transfer/src/disk_commit/thread.rs` (commit thread main loop and
  io_uring batch handle).
- `crates/transfer/src/disk_commit/process.rs` (per-file open / write / commit
  / fsync / rename / metadata pipeline).
- `crates/transfer/src/disk_commit/writer.rs` (`ReusableBufWriter`, the
  buffered+vectored writer that actually services every disk write today).
- `crates/transfer/src/disk_commit/config.rs` (`DiskCommitConfig`, including
  `do_fsync`, `temp_dir`, `io_uring_policy`).
- `crates/transfer/src/pipeline/spsc.rs` (lock-free SPSC channel).
- `crates/transfer/src/pipeline/messages.rs` (`FileMessage`, `BeginMessage`).
- `crates/transfer/src/temp_guard.rs` (named temp file naming + RAII guard, used
  for the temp+rename commit strategy).
- `crates/engine/src/local_copy/executor/file/guard.rs`
  (`DestinationWriteGuard`, the local-copy temp/anon-tmpfile guard).
- `crates/engine/src/local_copy/executor/file/copy/transfer/{execute,finalize}.rs`
  (local-copy commit driver).
- `crates/fast_io/src/io_uring/{mod,disk_batch,batching,config,file_writer}.rs`
  (existing io_uring plumbing).
- `crates/fast_io/src/o_tmpfile/low_level.rs` (anonymous tmpfile + libc
  `linkat(2)` wrapper).
- `crates/fast_io/Cargo.toml` (`io-uring = "0.7"`).
- Upstream rsync 3.4.1 source under `target/interop/upstream-src/rsync-3.4.1/`
  (`receiver.c:recv_files`, `fileio.c:write_file`, `util1.c:robust_rename`).

## TL;DR

oc-rsync already has a single-threaded disk commit thread and an idle
`fast_io::IoUringDiskBatch` ring instance, but the ring is never plumbed into
the per-file commit path: every committed file pays 3-4 distinct syscalls
(`write` x N -> optional `fsync` -> `rename` (or `linkat`) -> close). The
io_uring 0.7 crate exposes `IORING_OP_WRITE`, `IORING_OP_FSYNC`,
`IORING_OP_RENAMEAT` (5.11+) and `IORING_OP_LINKAT` (5.15+), and supports
`IOSQE_IO_LINK` chaining so that a "write all -> fsync -> rename" sequence can
be submitted as one chain and reaped in one batch CQE drain. Across N files we
can amortize the cost into one `io_uring_enter` per N files. The cost saving is
real for small-file workloads, but the correctness surface is large: chain
failure semantics (`ECANCELED` cascades), `--inplace` truncation via
`set_len()` (no io_uring opcode), `--partial-dir` cross-directory rename,
`--backup` rename-before-overwrite, and the `O_TMPFILE` + `linkat(AT_SYMLINK_FOLLOW)`
path that resolves a `/proc/self/fd/N` symlink (which `IORING_OP_LINKAT` may
not honour) all need explicit handling. **Recommendation: prototype, do not
implement on master.** The right scope is a single Linux 6.0+ feature flag, a
small `fast_io::commit_batch` helper that drives chained SQEs for the
write+fsync+rename triple, and a benchmark gate before wiring it into
`process_file`. Do not extend the chain across multiple files in v1; ordering
guarantees and per-file error reporting matter more than the marginal extra
syscall savings.

## Upstream evidence

Upstream rsync 3.4.1 has no io_uring path. `fileio.c:write_file` uses a single
static buffer (`wf_writeBuf`, 256 KB) and plain `write(2)`; per-file commit is
`flush -> close -> robust_rename` (`util1.c`). Upstream therefore has no
expectation that the wire protocol or transfer behaviour observe write-batch
semantics; any io_uring batching is purely a local optimisation that must be
indistinguishable from the read/write path in observable behaviour (final
on-disk byte content, file permissions, mtime, presence/absence of partial
files on error, `--fsync` durability guarantee).

## 1. Current commit path

oc-rsync runs a dedicated `disk-commit` thread (`crates/transfer/src/disk_commit/thread.rs:47-50`)
that consumes `FileMessage` items from a bounded lock-free SPSC channel
(`crates/transfer/src/pipeline/spsc.rs:150-157`, `crates/transfer/src/pipeline/messages.rs:21-45`).
The producer (network thread) sends one of `Begin -> Chunk* -> Commit`,
`WholeFile { begin, data }` (coalesced single-chunk fast path), `Abort`, or
`Shutdown`.

### 1.1 Per-file syscall sequence (chunked)

`process_file` (`crates/transfer/src/disk_commit/process.rs:26-126`) runs
per-file as follows:

1. `open_output_file` (`process.rs:199-223`) opens the output:
   - device target: `OpenOptions::new().write(true).open(file_path)` -> 1
     `openat(2)`.
   - `--inplace`: `OpenOptions::new().write(true).create(true).truncate(false).open(file_path)`
     plus optional `seek(SeekFrom::Start(append_offset))` -> 1 `openat`,
     optionally 1 `lseek`.
   - default temp+rename: `temp_guard::open_tmpfile`
     (`crates/transfer/src/temp_guard.rs:122-165`) which loops up to 100 times
     calling `OpenOptions::new().write(true).create_new(true).open(template)`
     until a unique `.filename.XXXXXX` succeeds -> 1 `openat` per attempt
     (`O_EXCL`).
2. Wraps the file in a `ReusableBufWriter` (`disk_commit/writer.rs:63-80`)
   that owns a 256 KB reusable buffer matching upstream's `wf_writeBufSize`.
3. For each `FileMessage::Chunk(data)`:
   - optional checksum update (`process.rs:62-64`).
   - `output.write_all(&data)` -> calls `ReusableBufWriter::write`
     (`writer.rs:82-105`):
     - chunks `>= DIRECT_WRITE_THRESHOLD` (8 KB, `writer.rs:23`) bypass the
       buffer and either `write_all_vectored` (combined buffered+chunk
       `writev`, `writer.rs:27-57`) or `file.write_all` -> 1 `writev` or 1
       `write` syscall per direct-mode chunk.
     - small chunks coalesce into the 256 KB buffer; full buffer triggers one
       `write_all` of the buffer contents.
   - the now-empty `Vec<u8>` is sent back through `buf_return_tx`
     (`process.rs:74`) so the network thread can reuse it (matches upstream
     static `wf_writeBuf` behaviour).
4. On `FileMessage::Commit` (`process.rs:76-102`):
   - sparse finish (zero-run punch, optional).
   - `flush_and_sync` (`process.rs:226-240`):
     - flush -> 1 `write(2)` if buffer is non-empty.
     - if `do_fsync`: `file.sync_all()` -> 1 `fsync(2)`.
   - `drop(output)` -> close fd (`close(2)`).
   - `commit_file` (`process.rs:243-271`):
     - optional `make_backup` (`process.rs:356-376`): 1 `stat` + `mkdir_all` +
       1 `rename(2)` if a backup is configured and the destination exists.
     - if `needs_rename` (default temp path): `fs::rename(temp, dest)` -> 1
       `renameat2(2)` (`std::fs::rename` uses `renameat2` on Linux).
     - if `--inplace`: reopens the destination
       (`OpenOptions::new().write(true).open`) and calls `file.set_len(final_size)`
       -> 1 extra `openat`, 1 `ftruncate(2)`, 1 `close(2)`.
   - `apply_post_commit_metadata` (`process.rs:277-297`) walks the
     `apply_metadata_from_file_entry` / ACL / xattr pipeline -> several
     `fchmod`/`fchown`/`utimensat`/`setxattr` syscalls per file.

### 1.2 Per-file syscall sequence (whole file)

`process_whole_file` (`process.rs:132-180`) is the single-buffer fast path: it
performs the same `open_output_file -> write_all -> flush_and_sync ->
commit_file -> apply_post_commit_metadata` sequence with one channel send
instead of three. The syscall count per file is the same.

### 1.3 Local-copy commit path (separate driver)

The local-copy executor uses a different commit driver. `execute_transfer`
(`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:46-573`)
constructs a `DestinationWriteGuard` (`guard.rs:113-217`) which can be:

- `GuardStrategy::NamedTempFile` (`guard.rs:62-67`, default): `commit` calls
  `commit_named_temp_file` -> `fs::rename` with retry on `EEXIST` /
  `ETXTBSY` and a cross-device fallback to `fs::copy` + `fs::remove_file`
  (`guard.rs:305-342`).
- `GuardStrategy::Anonymous` (Linux only, `guard.rs:72-76`): backed by
  `fast_io::open_anonymous_tmpfile` (`O_TMPFILE`); `commit_anonymous`
  (`guard.rs:348-361`) removes any existing destination then calls
  `fast_io::link_anonymous_tmpfile`
  (`crates/fast_io/src/o_tmpfile/low_level.rs:185-224`), which issues
  `libc::linkat(AT_FDCWD, "/proc/self/fd/N", AT_FDCWD, dest, AT_SYMLINK_FOLLOW)`.

Local-copy commit therefore performs the same write+rename (or write+linkat)
syscall pattern but lives outside the `disk_commit` thread; any io_uring
batching scheme that targets `disk_commit` will not naturally cover this path.

### 1.4 Existing batching

- Within a single file, large chunks (>= 8 KB) collapse adjacent buffered data
  + new chunk into one `writev` (`writer.rs:27-57`). This is the only batching
  active today.
- Across files, no batching: each file has its own `open -> write* -> flush
  -> [fsync] -> close -> rename -> chmod/utimens` sequence ordered by the
  channel.
- The disk thread allocates an `IoUringDiskBatch`
  (`crates/transfer/src/disk_commit/thread.rs:127`, named `_disk_batch`) but
  **never plumbs it into `process_file` or `process_whole_file`**. The handle
  is only used to flip the diagnostic log line in `log_io_uring_status`
  (`thread.rs:85-112`). The ring is allocated, registered files registration
  attempted, and then dropped at thread exit unused. This is dead
  infrastructure today.

## 2. io_uring batching model

### 2.1 Opcodes available in io-uring 0.7

oc-rsync depends on `io-uring = "0.7"` (`crates/fast_io/Cargo.toml:36`). The
opcodes relevant to a commit-chain are:

| Opcode | Min kernel | Use |
|--------|------------|-----|
| `IORING_OP_WRITE` (`opcode::Write`) | 5.6 | Per-chunk / per-buffer file write at offset. Already used by `IoUringDiskBatch::flush_current` (`disk_batch.rs:207-236`) via `submit_write_batch` (`batching.rs:53-143`). |
| `IORING_OP_WRITEV` (`opcode::Writev`) | 5.6 | Vectored write; would let us merge buffered + new-chunk like `write_all_vectored` does today. |
| `IORING_OP_FSYNC` (`opcode::Fsync`) | 5.6 | Full or data-only fsync (`IORING_FSYNC_DATASYNC` flag). Used today by `IoUringDiskBatch::submit_fsync` (`disk_batch.rs:239-266`). |
| `IORING_OP_CLOSE` (`opcode::Close`) | 5.6 | Async `close(2)`. Useful for the final drop of the writer fd on commit. |
| `IORING_OP_RENAMEAT` (`opcode::RenameAt`) | 5.11 | Async `renameat2(2)`. Required for both temp+rename commits and `--backup`. Not currently wired in `fast_io`. |
| `IORING_OP_LINKAT` (`opcode::LinkAt`) | 5.15 | Async `linkat(2)`. Would replace the `libc::linkat` call inside `link_anonymous_tmpfile`. Not currently wired. Note: passing `AT_SYMLINK_FOLLOW` plus a `/proc/self/fd/N` source is the only way to materialise an `O_TMPFILE` inode; the io_uring opcode supports the same flags as the syscall, but kernel support for resolving the procfs symlink under io_uring should be confirmed at runtime via `IORING_REGISTER_PROBE`. |
| `IORING_OP_UNLINKAT` (`opcode::UnlinkAt`) | 5.11 | Async `unlinkat(2)`. Useful for `--backup` cleanup or pre-commit removal of an existing destination before `linkat`. |
| `IORING_OP_FALLOCATE` (`opcode::Fallocate`) | 5.6 | Async preallocate; out of scope here, called by sender preallocation today. |

### 2.2 IOSQE_IO_LINK chaining

`IOSQE_IO_LINK` (set via `Entry::flags(io_uring::squeue::Flags::IO_LINK)` in
the 0.7 API) makes the **next** SQE in submission order depend on the
**successful completion** of the current SQE. Behaviour:

- The chain submits as a single batch (`io_uring_enter` once for the chain).
- If a linked SQE succeeds, the next runs.
- If a linked SQE fails (CQE result < 0), every later SQE in the chain
  completes with `-ECANCELED`.
- A "short" result (e.g. `IORING_OP_WRITE` returning fewer bytes than asked)
  is **not** treated as failure for chain purposes; the chain advances. The
  caller has to detect short writes manually and re-submit the remainder.
  `IOSQE_IO_HARDLINK` (5.5+) is the variant that propagates regardless of
  success, but it is not what we want for commit semantics: a failed write
  must abort the rename.

For the commit triple `write_all -> fsync -> rename` we want:

```
write_0    [LINK]
write_1    [LINK]
...
write_n-1  [LINK]   (no LINK on last write if fsync omitted)
fsync      [LINK]   (only when --fsync requested)
rename     (no LINK; tail of chain)
```

A successful chain produces n+2 CQEs. On failure of any write (short or
errored), the dependent fsync and rename complete with `-ECANCELED`, and we
discard the temp file - this matches today's "drop guard, remove temp"
recovery on write error.

### 2.3 What is already wired in fast_io

`crates/fast_io/src/io_uring/`:

- `IoUringDiskBatch` (`disk_batch.rs:45-303`): owns one ring, supports
  `begin_file -> write_data* -> commit_file(do_fsync)`. Internally it uses
  `submit_write_batch` (a multi-SQE writer that submits up to `sq_entries`
  parallel chunks per `io_uring_enter`) and a separate `submit_fsync`. **It
  does not chain SQEs across the write+fsync boundary**; the fsync is its own
  `submit_and_wait(1)`. It also has no rename or link opcode.
- `IoUringWriter` (`file_writer.rs`): per-file writer with `WRITE_FIXED` /
  registered-buffer support. Built for the sender side, not the commit thread.
- `submit_write_batch` (`batching.rs:53-143`): chunks data into multiple
  parallel `IORING_OP_WRITE` SQEs, but the SQEs are **independent** (no
  `IO_LINK`). This works for a single file's contiguous write but does not
  enforce ordering needed for write -> fsync -> rename.
- `IORING_REGISTER_FILES` is exercised but `register_files`/
  `unregister_files` is called per `begin_file`, meaning each file pays a
  registration syscall pair. For an N-file batch we want to either skip
  registration on small batches or re-use a fixed slot bank.

What is **not** wired:

- `IORING_OP_RENAMEAT` / `IORING_OP_LINKAT` / `IORING_OP_UNLINKAT` /
  `IORING_OP_CLOSE` - none of these are used by oc-rsync today.
- `IOSQE_IO_LINK` chaining - no current call site sets the flag.
- A "commit chain" helper in `fast_io` that publishes `(write_buffer, do_fsync,
  rename_from, rename_to)` -> single chained submission.

## 3. Per-file vs batched-N-files

We have two orthogonal axes for batching: within a single file (the chain
`write -> fsync -> rename`), and across files (multiple commit chains submitted
together).

### 3.1 Single-file chain

Today, per-file commit costs:

- temp+rename, no `--fsync`: `openat(temp)` + k * `write(2)` (or `writev` /
  `pwrite`) + `close(2)` + `renameat2(2)` = (3 + k) syscalls minimum.
  `k = 1` for a small file using `process_whole_file` and the direct-write
  path, so the theoretical minimum today is 4 syscalls.
- temp+rename, with `--fsync`: add 1 `fsync(2)`. Minimum 5 syscalls.
- inplace, with `--fsync`: `openat` + write(s) + `fsync` + `close` + `openat
  (for set_len)` + `ftruncate` + `close` -> minimum 7 syscalls.
- `O_TMPFILE` + linkat path (local copy only today): `O_TMPFILE openat` +
  write(s) + optional `fsync` + `unlink(dest)` if extant + `linkat`. Minimum
  4-5 syscalls.

A linked io_uring chain of `write -> fsync -> rename` reduces the user-space
syscall count from 3 (write, fsync, rename) to 1 `io_uring_enter` plus 1
`io_uring_enter` for completion drain (or 1 if `submit_and_wait` is used with
the chain length). The `openat` and `close` remain outside the ring unless we
also add `IORING_OP_OPENAT` (5.6) and `IORING_OP_CLOSE` (5.6) - both are
supported but pull file lifetime into io_uring user_data, complicating error
recovery.

Net: 3-5 syscalls -> 2 syscalls per file for the temp+rename+fsync case. The
user-space CPU savings are dominated by removing the per-syscall context
switch (~100-200 ns each on a modern x86_64) and removing the libc wrapper
overhead. For a small-file workload (10 KB files), that is a meaningful
fraction of per-file cost; for large files (100 MB+), per-file commit cost is
negligible compared to the GB of writes.

### 3.2 Cross-file batching (N files per submit)

If we accumulate M files' commit chains and submit them in one batch, the
amortisation goes from "1 enter per chain" to "1 enter per M chains plus 1
drain per M chains". The drain cost is essentially M * 4-byte CQE reads from
mmaped memory plus the dispatch decision per CQE, which is dominated by cache
behaviour.

Current = 3-4 syscalls per file (write + optional fsync + rename + close);
batched-N = 1 `io_uring_enter` for submit + 1 for drain per N files. For
`N = 16` and a small-file workload, that is roughly 64 syscalls -> 2 syscalls
across 16 files, i.e. 32x fewer syscalls. Empirically the saving in CPU
instructions on a small-file workload (`cp`-style copy of many tiny files) on
io_uring is usually 20-40% in user time, but disk-bound workloads see <5%
because the bottleneck is the device.

The hard limits on cross-file batching:

- **Submission queue depth.** `IoUringConfig::sq_entries` is 64 by default
  (`config.rs`). Each chain consumes 2-3 SQE slots
  (writes + fsync + rename), so M is capped near 16-32 in-flight files.
- **Memory pressure.** Each pending chain holds a reference to the buffer
  about to be written. With M files in flight and 256 KB buffers, that is up
  to M * 256 KB peak memory. For M = 32 that is 8 MB, fine. For M = 4096
  (the channel cap) that is 1 GB, not fine.
- **Error reporting.** Today the disk thread sends one `CommitResult` per
  file. Batching M files means we must drain M completion sets before sending
  any acks back, which delays the network thread's ability to free buffers and
  may stall the SPSC channel.

For the v1 design we recommend single-file chains only, and leave cross-file
batching as a follow-up after the per-file chain is benchmarked.

## 4. Risks

### 4.1 Partial chain failure semantics

When a write SQE in the chain returns a short result, io_uring does **not**
fail the chain - the linked `fsync` and `rename` will still execute on a
half-written file. The caller must inspect each CQE's result, detect the short
write, and either resubmit the remainder before the rename proceeds or cancel
the chain post-hoc. The simpler design is:

- Submit only the writes with `IO_LINK`.
- After all write CQEs are reaped and verified complete, submit the
  fsync+rename chain separately.

That gives up the "single submit per file" property but preserves correctness.
The full single-shot chain is only safe when the write is small enough to fit
in one SQE and we can rely on the kernel's full-write guarantee for sub-page
buffered writes (it is not safe in general).

### 4.2 Ordering guarantees

`IOSQE_IO_LINK` enforces ordering only within a chain. Files submitted in
separate chains in the same submission may complete out of order, which is
fine for unrelated files but breaks if the same destination path is touched
twice (e.g. `--backup` rename followed by new file write). The disk thread
processes files sequentially, so today there is no cross-file ordering
hazard, but any cross-file batching scheme needs a per-path linearization
check or a "commit chain barrier" between conflicting chains.

### 4.3 fsync semantics across files

Upstream rsync's `--fsync` semantics fsync each file individually before
acknowledging it. A batched scheme that defers fsync until M files have
completed their writes would change the durability story: on a crash between
"write done" and "fsync issued", upstream loses 1 file; oc-rsync would lose up
to M files. We must not defer fsync across files. Per-file fsync inside the
chain preserves the upstream semantic.

### 4.4 Fallback path for non-io_uring kernels

`fast_io::is_io_uring_available()` (`io_uring/config.rs:167-180`) caches the
runtime probe result in a process-wide atomic. The commit-chain helper must:

- On `Disabled` policy: never construct the ring; `process_file` calls the
  existing `ReusableBufWriter` + `flush_and_sync` + `commit_file` path.
- On `Auto` policy: try ring construction once at thread start; on success,
  use the chain helper; on failure, fall through to the read/write path.
- On `Enabled` policy: matches `Auto` today (`thread.rs:71-77` does not error
  out on ring construction failure even with `Enabled`).

A second probe is needed for `RENAMEAT` / `LINKAT` opcode support: kernel 5.6
satisfies the existing `MIN_KERNEL_VERSION` (`config.rs:19`) but does not
support either of those opcodes. We should call `IORING_REGISTER_PROBE`
(already plumbed via `count_supported_ops`, `config.rs:246-253`) and only
enable the chain when the specific opcodes are reported supported. The
existing `IoUringKernelInfo::supported_ops` field is just a count; we must
extend it to record op-by-op availability or add a per-op `is_supported`
helper.

### 4.5 Interaction with `--inplace`

`--inplace` is the most invasive case. The current path is:

1. Open destination `O_WRONLY|O_CREAT` (no truncate).
2. Optional seek to `append_offset`.
3. Write all chunks.
4. Optional fsync.
5. Drop the file (close).
6. Reopen the destination, call `file.set_len(target_size)`, drop again
   (`process.rs:262-268`).

Step 6 has no direct io_uring opcode (`IORING_OP_FALLOCATE` covers `fallocate`
but not `ftruncate`). Possible options:

- Keep `set_len` on the standard syscall path. The chain becomes
  `write -> [fsync]`, then a synchronous `set_len`, then optional second fsync.
  This still saves the per-write enter overhead.
- Use `IORING_OP_FALLOCATE` with `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`
  for shrinking, which is only valid for sparse files and is not a general
  truncation primitive. Reject this option.

Recommendation: scope v1 to non-inplace files only, fall back for `--inplace`.

### 4.6 Interaction with `--partial-dir`

`--partial-dir` puts the partial file in a separate directory
(`crates/engine/src/local_copy/executor/file/paths.rs`,
`partial_directory_destination_path`). The temp file lives in the
`--partial-dir`, but the final destination is in the originating directory.
`renameat2(2)` can rename across directories on the same filesystem, so
`IORING_OP_RENAMEAT` works here. If the partial dir is on a **different**
filesystem, the current code falls back to `fs::copy + fs::remove_file`
(`guard.rs:324-336`); this fallback cannot live inside an io_uring chain.
Detection has to happen before chain submission via the source/dest dirfd
comparison or by attempting the chain and falling back on `EXDEV`.

### 4.7 Interaction with `O_TMPFILE` + linkat

The local-copy `Anonymous` strategy
(`crates/engine/src/local_copy/executor/file/guard.rs:225-243`) materialises
the inode via `link_anonymous_tmpfile`
(`crates/fast_io/src/o_tmpfile/low_level.rs:185-224`) which calls
`libc::linkat(AT_FDCWD, "/proc/self/fd/N", AT_FDCWD, dest, AT_SYMLINK_FOLLOW)`.
`IORING_OP_LINKAT` (5.15+) accepts the same flags, but kernels have at
various points been stricter about following procfs symlinks under io_uring.
Until verified across the supported kernel matrix, the safest choice is to
keep `link_anonymous_tmpfile` on the libc path and only batch the regular
temp+rename case.

The local-copy path also lives in a different thread (the executor itself,
not `disk_commit`), so any io_uring batching of the local-copy commit needs a
parallel design and is out of scope for task #1086.

### 4.8 Buffer lifetime under linked submission

`IORING_OP_WRITE` reads from a user buffer that must remain valid until the
CQE is observed. Today `submit_write_batch` (`batching.rs:53-143`) holds a
`&[u8]` slice until `submit_and_wait` returns, which is correct. A chained
`write -> fsync -> rename` submission must keep the same slice alive until
the **rename CQE** is observed - longer than the existing helper holds it.
That means the buffer cannot be returned to the network thread via
`buf_return_tx` until commit completes, slightly increasing the steady-state
pool size. With `WRITE_FIXED` + registered buffers the buffer lifetime is
tied to the slot bank, which simplifies the dance but adds the slot-bank
constraint (number of in-flight writes <= bank size).

### 4.9 Cross-platform

`disk_commit/process.rs` is unit-tested on macOS and Windows today via the
fallback path. The chain helper must be `#[cfg(target_os = "linux")]` with a
no-op stub on other platforms. macOS and Windows will continue to use the
read/write path. There is no platform-specific behaviour change.

## 5. Findings

### F1. Disk-commit thread allocates `IoUringDiskBatch` but never uses it

**Severity**: Medium.

**Evidence**: `crates/transfer/src/disk_commit/thread.rs:127`:

```rust
let mut _disk_batch = try_create_disk_batch(config.io_uring_policy);
log_io_uring_status(config.io_uring_policy, _disk_batch.is_some());
```

`process_file` (`disk_commit/process.rs:26-126`) and `process_whole_file`
(`process.rs:132-180`) never receive `_disk_batch` and unconditionally drive
writes through `ReusableBufWriter` (`writer.rs`).

**Impact**: Misleading. The log line at `-vv` reports "io_uring: enabled
(kernel X.Y, K ops supported)" implying io_uring is active, but every write
goes through `std::fs::File::write_all`. Users reading `--debug io2` output
will misattribute performance to the io_uring path. There is also a real cost
to ring construction (one `io_uring_setup(2)` per transfer, plus optional fd
registration).

**Recommended fix**: Either plumb the batch into `process_file` (the subject of
this audit) or stop allocating the ring and update the log line to
"io_uring: enabled (not yet wired into commit path)". The latter is the
correct intermediate state until the prototype lands.

### F2. `IoUringDiskBatch::commit_file` does not chain fsync after writes

**Severity**: Low (latent).

**Evidence**: `crates/fast_io/src/io_uring/disk_batch.rs:170-190` calls
`flush_current()` (which `submit_and_wait`s for all writes) and then
`submit_fsync` (a separate `submit_and_wait(1)`).

**Impact**: Even if the disk thread were to use `IoUringDiskBatch`, fsync
would cost an extra `io_uring_enter`. With `IO_LINK`, the fsync SQE could be
linked behind the last write SQE in a single submit, halving the syscall
count in the `--fsync` path.

**Recommended fix**: Add a `IoUringDiskBatch::commit_with_chain` variant that
chains the trailing write SQE -> fsync (when requested). Keep the current
non-chained variant for callers that need explicit fsync error handling.

### F3. No io_uring rename / link opcodes in fast_io

**Severity**: Medium.

**Evidence**: A grep across `crates/fast_io/` for `RenameAt`, `LinkAt`,
`opcode::Rename`, `opcode::Link` returns zero matches. `link_anonymous_tmpfile`
(`o_tmpfile/low_level.rs:185-224`) calls `libc::linkat` directly. The named
temp+rename path drops to `std::fs::rename` (`process.rs:256` /
`guard.rs:308`).

**Impact**: A commit chain cannot include the rename or linkat step today.
The maximum batching achievable is `write -> fsync` per file, and the rename
must always be a separate libc call, costing one extra syscall per file.

**Recommended fix**: Add `IORING_OP_RENAMEAT` and `IORING_OP_LINKAT`
helpers in `fast_io::io_uring`, gated on a runtime probe. Wire them into a
new `commit_chain` function that submits write batch + optional fsync + final
rename or linkat in a single chained submission. Keep the libc fallbacks for
older kernels.

### F4. No per-opcode runtime probe

**Severity**: Medium.

**Evidence**: `is_io_uring_available` (`io_uring/config.rs:167-180`) caches a
boolean. `count_supported_ops` (`config.rs:246-253`) returns a count but the
public API only exposes the count, not which opcodes are present. The chain
helper needs `IORING_OP_RENAMEAT` (5.11+) and `IORING_OP_LINKAT` (5.15+)
specifically; a kernel that satisfies the 5.6 minimum may lack them.

**Impact**: Without per-opcode probing, the chain helper cannot safely
auto-detect availability and would have to bump the global minimum from 5.6
to 5.15, breaking older but otherwise supported kernels.

**Recommended fix**: Extend `IoUringKernelInfo` with an `op_supported(opcode)
-> bool` accessor backed by the cached `Probe`. Use it from
`commit_chain::is_supported()` to decide between chained and serial commit.

### F5. Buffer recycling protocol must extend across chain lifetime

**Severity**: Medium.

**Evidence**: `process.rs:73-74` returns the chunk `Vec<u8>` to the network
thread immediately after `output.write_all(&data)`. With chain submission,
`write_all` returns when the CQE for the write SQE is observed; chains link
write -> fsync -> rename, so the SQE chain is in flight until the rename CQE
is reaped. If we recycle on write completion but the linked fsync fails and
we abort, the chunk buffer has already been re-used on the network side -
fine for the abort itself, but it makes any retry-the-same-chunk strategy
impossible.

**Impact**: Today's "abort means discard temp file and report error" is
sufficient because writes are sequential. A chain-aware design must keep
buffer ownership inside the disk thread until commit completes, then bulk-
return buffers via `buf_return_tx`. This raises the steady-state buffer pool
size by roughly `chain_depth * avg_chunk_size`.

**Recommended fix**: Document the new lifetime requirement in
`disk_commit/messages.rs` and `process.rs`. Defer the `buf_return_tx.send`
calls until after the commit CQE is observed. Size the channel capacity
accordingly (`DEFAULT_CHANNEL_CAPACITY` is 128 today, which already provides
plenty of headroom).

### F6. `--inplace`, `--partial-dir`, and `O_TMPFILE` paths are not chain-friendly

**Severity**: Low (scope guidance).

**Evidence**: `--inplace` requires a post-write `set_len` (`process.rs:266-267`)
that has no io_uring opcode. `--partial-dir` may straddle filesystems, hitting
the `EXDEV` fallback (`guard.rs:324-336`). `O_TMPFILE` materialisation goes
through `linkat(AT_SYMLINK_FOLLOW, /proc/self/fd/N, ...)`
(`o_tmpfile/low_level.rs:185-224`), whose io_uring behaviour across kernels
is not validated in this codebase.

**Impact**: A general "all commits go through io_uring chain" change is
unsafe for these cases.

**Recommended fix**: Restrict the v1 chain helper to the temp+rename, regular
file, no-`--inplace` path. Detect the disqualifying conditions in
`process_file` before invoking the chain and route to the existing path
otherwise.

## 6. Recommendation

**Prototype (do not implement on master in v1).** Build the chain helper as a
separate, gated, benchmark-validated module before changing `process_file`.

Concrete next steps:

1. **#TBD-A:** Add per-opcode probing to `fast_io::io_uring::config` so callers
   can ask "is `IORING_OP_RENAMEAT` available?" without reprobing. Keep the
   global `MIN_KERNEL_VERSION` at 5.6 and have the chain helper require
   `RENAMEAT` (5.11) and optionally `LINKAT` (5.15) at runtime. (Findings F4,
   F3.)
2. **#TBD-B:** Implement `fast_io::io_uring::commit_chain` exposing
   `submit_commit(file_fd, buffers, do_fsync, rename_from_dirfd, rename_from_name,
   rename_to_dirfd, rename_to_name) -> io::Result<()>`. Internally:
   - For each chunk, push `Write` SQE with `IO_LINK`.
   - If `do_fsync`, push `Fsync` SQE with `IO_LINK`.
   - Push `RenameAt` SQE without `IO_LINK` (tail of chain).
   - Single `submit_and_wait(chain_len)`.
   - Reap each CQE; on any `result < 0`, return the first error and rely on
     the `ECANCELED` cascade for the rest.
   Linux-only; no-op stub on other platforms. (Findings F2, F3.)
3. **#TBD-C:** Add a Criterion benchmark
   `crates/fast_io/benches/disk_commit_chain.rs` with three scenarios:
   - 1000 small files (4 KB), no fsync, temp+rename.
   - 1000 small files, with fsync.
   - 100 medium files (4 MB), with fsync.
   Compare `commit_chain` against today's `ReusableBufWriter` +
   `fs::rename` path. Acceptance threshold for moving forward: >= 15%
   reduction in user CPU time on the small-file no-fsync scenario.
4. **#TBD-D:** Wire `commit_chain` into `disk_commit::process_file` /
   `process_whole_file` behind a single eligibility check
   (regular file, temp+rename strategy, no `--inplace`, supported kernel).
   Fall back to the existing `ReusableBufWriter` + `commit_file` path on any
   disqualifier, including `EXDEV` failure on rename. Stop allocating
   `_disk_batch` outside this path. (Findings F1, F6.)
5. **#TBD-E:** Extend `BeginMessage` lifetime semantics so chunk buffers are
   not returned via `buf_return_tx` until the commit CQE is observed. Update
   `messages.rs` doc and add a unit test that races a fast network producer
   against a slow disk thread with chained submission. (Finding F5.)
6. **#TBD-F:** Update `log_io_uring_status` (`thread.rs:85-112`) to report
   the actual mode in use ("io_uring chained commit", "io_uring writes only",
   "standard I/O"), not just whether the ring constructed. (Finding F1.)
7. **Deferred to a follow-up audit:** cross-file batching (M files per
   `io_uring_enter`), `O_TMPFILE` + `linkat` chain for the local-copy
   executor, `IORING_OP_OPENAT` / `IORING_OP_CLOSE` to bring `open` and
   `close` into the chain.

The prototype must demonstrate on the bench in step 3 that the saving is real
on the small-file no-fsync workload before any merge to master. If the saving
is below the threshold, close the task with the benchmark numbers as evidence
and document the result here; do not ship dead infrastructure.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (`receiver.c:recv_files`, `fileio.c:write_file`,
  `util1.c:robust_rename`).
- io_uring opcode reference: `io_uring(7)` man page; io-uring 0.7 crate docs
  (https://docs.rs/io-uring/0.7).
- Linked SQE semantics: `io_uring_enter(2)` man page section "Linked SQEs"
  and `IOSQE_IO_LINK` flag definition.
- Kernel opcode introduction commits:
  - `IORING_OP_RENAMEAT`: Linux v5.11.
  - `IORING_OP_LINKAT`: Linux v5.15.
  - `IORING_OP_UNLINKAT`: Linux v5.11.
  - `IORING_OP_CLOSE`, `IORING_OP_FSYNC`, `IORING_OP_WRITE`: Linux v5.6.
- Existing oc-rsync io_uring infrastructure:
  `crates/fast_io/src/io_uring/` (modules `disk_batch`, `batching`, `config`,
  `file_writer`).
- SPSC commit pipeline: `crates/transfer/src/{pipeline/spsc.rs,
  pipeline/messages.rs, disk_commit/}`.
- Local-copy commit driver: `crates/engine/src/local_copy/executor/file/`.
