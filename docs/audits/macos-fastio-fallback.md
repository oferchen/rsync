# macOS fast_io fallback vs Linux io_uring (#1652)

Tracking issue: oc-rsync #1652. Static, source-grounded audit of the
macOS fallback chain in `crates/fast_io/` as of branch
`docs/macos-fastio-audit-1652` (forked from `origin/master`). No
benchmarks were collected; every quantitative claim is derived from
syscall semantics and the source itself.

Earlier audits on the same topic remain in tree and are referenced
where they overlap:

- `docs/audits/fast-io-macos-fallback.md` - first source walk.
- `docs/audits/fast-io-fallback-macos-vs-linux.md` - extended per-
  syscall budget.
- `docs/audits/fastio-macos-fallback.md` - companion design notes.
- `docs/audits/macos-dispatch-io.md` - #1653 dispatch_io evaluation.
- `docs/audits/bsd-aio.md` - #1655 POSIX aio evaluation.

The fresh contribution of this document is section 4: the
`MacosWriter` optimised path was landed but is not wired into any
caller in the workspace. That observation supersedes the "#1657
pending" language in the earlier audits.

## 1. macOS code paths under audit

Three call surfaces define the macOS fallback today.

### 1.1 `crates/fast_io/src/platform_copy/`

Whole-file copy dispatch on macOS lives in
`platform_copy/dispatch.rs`:

- `platform_copy_impl` (`platform_copy/dispatch.rs:63`) gated
  `#[cfg(target_os = "macos")]`. Tries, in order:
  - `clonefile_impl` (`platform_copy/dispatch.rs:151`) - wraps
    `libc::clonefile(src, dst, 0)` for APFS copy-on-write. Reports
    `CopyResult::new(0, CopyMethod::Clonefile)` because zero data is
    moved.
  - `fcopyfile_impl` (`platform_copy/dispatch.rs:186`) - opens both
    fds and calls `libc::fcopyfile(src_fd, dst_fd, NULL,
    COPYFILE_DATA)` for cross-volume copies and non-APFS targets.
  - `std::fs::copy` - portable userspace `read`/`write` loop.
- `platform_supports_reflink()`
  (`platform_copy/dispatch.rs:790`) returns `true` on macOS.
- `platform_preferred_method(_)`
  (`platform_copy/dispatch.rs:830`) returns
  `CopyMethod::Clonefile` regardless of size.

The public re-exports `try_clonefile` and `try_fcopyfile` live at
`platform_copy/mod.rs:264` and `platform_copy/mod.rs:291`. The
local-copy executor reaches the dispatch chain via
`PlatformCopy::copy_file` at
`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:170`
under `#[cfg(target_os = "macos")]`. Only zero-copy results are
accepted from this path; any data-copy fallback is discarded so the
delta machinery still runs.

### 1.2 `crates/fast_io/src/macos_io.rs`

Defines `MacosWriter`, an optimised buffered writer that pairs
`fcntl(fd, F_NOCACHE, 1)` for large files with `writev(2)` for
scatter-gather flushes.

- `F_NOCACHE_THRESHOLD = 1 MiB` (`macos_io.rs:34`). Below the
  threshold, the cache is left enabled because small files benefit
  from page cache hits.
- `MAX_IOV_COUNT = 64` (`macos_io.rs:41`). Conservative versus the
  POSIX `IOV_MAX = 1024` ceiling to keep iovec arrays off the stack
  hot path.
- `MacosWriter::create` (`macos_io.rs:84`) and
  `MacosWriter::from_file` (`macos_io.rs:105`) take a `size_hint`
  that drives the `F_NOCACHE` decision.
- `MacosWriter::try_set_nocache` (`macos_io.rs:137`) issues the
  `fcntl(F_NOCACHE, 1)` and tolerates failure silently - filesystems
  that reject the flag fall back to cached I/O without erroring.
- `MacosWriter::flush_writev` (`macos_io.rs:153`) drains pending
  buffers in batches of `MAX_IOV_COUNT` via `libc::writev`, with
  per-batch `EINTR` retry and an explicit `write_all_to_fd` recovery
  for partial-`writev` cases (`macos_io.rs:200`-`macos_io.rs:212`).
- Default flush threshold is 256 KiB (`macos_io.rs:97`), matching
  the disk-commit thread's `WRITE_BUF_SIZE` at
  `crates/transfer/src/disk_commit/writer.rs:20`.
- Standalone helpers: `writev_buffers` (`macos_io.rs:287`),
  `set_nocache` (`macos_io.rs:349`), `is_nocache_set`
  (`macos_io.rs:365`, set-only on macOS so always returns `false`).
- Stub variants for non-macOS (`macos_io.rs:377`-`macos_io.rs:455`)
  wrap a `BufWriter<File>` so callers compile cross-platform with
  no `#[cfg]` branching.

Re-exported via `crates/fast_io/src/lib.rs:155`-`crates/fast_io/src/lib.rs:157`:

```
pub use macos_io::{
    F_NOCACHE_THRESHOLD, MAX_IOV_COUNT, MacosWriter, is_nocache_set,
    set_nocache, writev_buffers,
};
```

### 1.3 `crates/fast_io/src/io_uring_stub.rs`

Compiled in place of the Linux io_uring module when either
`target_os != "linux"` or the `io_uring` cargo feature is off (see
the cfg-gated `pub mod io_uring;` selection at
`crates/fast_io/src/lib.rs:115`-`crates/fast_io/src/lib.rs:119`).
The stub exists to give callers a single API surface and never
constructs an actual ring.

Key surface, all returning `Unsupported` / `false`:

- `is_io_uring_available()` (`io_uring_stub.rs:25`) - always `false`.
- `IoUringConfig` (`io_uring_stub.rs:56`) - present for ABI parity.
- `IoUringReaderFactory::open` (`io_uring_stub.rs:1226`) - returns
  `IoUringOrStdReader::Std(StdFileReader::open(path)?)`, the
  `BufReader<File>` from `crates/fast_io/src/traits.rs:84`.
- `IoUringWriterFactory::create` (`io_uring_stub.rs:1332`) and
  `create_with_size` (`io_uring_stub.rs:1336`) - return
  `IoUringOrStdWriter::Std(StdFileWriter::create(path)?)`, the
  `BufWriter<File>` from `crates/fast_io/src/traits.rs:127`.
- `writer_from_file` (`io_uring_stub.rs:1347`) /
  `writer_from_file_with_depth` (`io_uring_stub.rs:1360`) - wrap an
  existing fd in `StdFileWriter::from_file_with_capacity`
  (`crates/fast_io/src/traits.rs:136`). If `IoUringPolicy::Enabled`
  was requested, an explicit `Unsupported` error is returned so the
  user sees that `--io-uring` did nothing.
- `IoUringDiskBatch::new` (`io_uring_stub.rs:913`),
  `try_new` (`io_uring_stub.rs:921`), `begin_file`
  (`io_uring_stub.rs:926`), `write_data` (`io_uring_stub.rs:934`),
  `commit_file` (`io_uring_stub.rs:950`) - every method returns
  `Unsupported`. The struct exists only so that the type-checked
  `Writer::IoUring { batch }` arm in `disk_commit/writer.rs` can be
  named in cross-platform code.
- `statx_supported()` (`io_uring_stub.rs:628`) always `false`;
  `submit_statx_batch` (`io_uring_stub.rs:658`) returns
  `Unsupported` per path.
- `linkat_supported()` and `renameat2_supported()` analogues are
  similarly stubbed.

Net: on macOS, every io_uring entry point falls back to
`StdFileReader` / `StdFileWriter`. The macOS `MacosWriter` is not
substituted in at any stub site.

## 2. Linux to macOS fallback mapping

| Linux io_uring path | macOS fallback in tree |
|---|---|
| `IoUringDiskBatch` (disk-commit batched writes, `crates/fast_io/src/io_uring/disk_batch.rs`) | `Writer::Buffered(ReusableBufWriter)` at `crates/transfer/src/disk_commit/writer.rs:142` - synchronous `write`/`write_vectored` on a 256 KiB reusable buffer. |
| `IoUringWriter::create` / `writer_from_file` (`crates/fast_io/src/io_uring/file_writer.rs`) | `StdFileWriter` (`crates/fast_io/src/traits.rs:120`) - `BufWriter<File>`, 8 KiB default capacity. |
| `IoUringReader::open` / `reader_from_path` (`crates/fast_io/src/io_uring/file_reader.rs`) | `StdFileReader` (`crates/fast_io/src/traits.rs:76`) - `BufReader<File>`. |
| `copy_file_range::copy_file_contents_buffered` whole-file zero-copy (`crates/fast_io/src/copy_file_range.rs:110`) | `copy_file_contents_readwrite_with_buffer` (`crates/fast_io/src/copy_file_range.rs:392`) - `read`/`write` loop with the pool's buffer. |
| `FICLONE` ioctl (`crates/fast_io/src/platform_copy/dispatch.rs:709`) | `clonefile_impl` (`crates/fast_io/src/platform_copy/dispatch.rs:151`) - APFS CoW. |
| `sendfile(2)` file-to-socket zero-copy (`crates/fast_io/src/sendfile.rs:147`) | `copy_via_fd_write` loop (`crates/fast_io/src/sendfile.rs:158`). macOS has its own `sendfile(2)` with a different signature; the fast_io crate does not invoke it. |
| `splice(2)` socket-to-file zero-copy (`crates/fast_io/src/splice.rs`) | None. Returns `Unsupported`; macOS has no `splice`. |
| `IORING_OP_STATX` batched metadata (`crates/fast_io/src/io_uring/statx.rs`) | `execute_metadata_ops` (`crates/fast_io/src/syscall_batch.rs:132`) - sequential `std::fs::metadata` / `symlink_metadata` calls. |
| `IORING_OP_LINKAT` / `IORING_OP_RENAMEAT` (`crates/fast_io/src/io_uring/linkat.rs`, `renameat2.rs`) | `std::fs::hard_link`, `std::fs::rename`. |
| `O_TMPFILE` anonymous temp file (`crates/fast_io/src/o_tmpfile/low_level.rs:30`) | Stub at `crates/fast_io/src/o_tmpfile/low_level.rs:331` returns `Unsupported`; named temp file path is used. |
| Registered buffers, SQPOLL, provided buffer ring (`crates/fast_io/src/io_uring/registered_buffers.rs`, `shared_ring.rs`, `buffer_ring.rs`) | No analogue. |
| `IOCP` (Windows) | Symmetric stub at `crates/fast_io/src/iocp_stub.rs`. |

## 3. Performance gap inventory

### 3.1 Whole-file copy: good

On APFS the path is structurally optimal. `clonefile(2)` is O(1) and
moves zero data; `fcopyfile(3)` is one kernel call for cross-volume
or HFS+ targets. The `PlatformCopy` chain at
`platform_copy/dispatch.rs:62`-`platform_copy/dispatch.rs:94` is
short and well-ordered. No gap to close here.

### 3.2 Disk-commit write path: synchronous

`Writer::Buffered` in `crates/transfer/src/disk_commit/writer.rs:142`
is the only macOS path. `make_writer`
(`crates/transfer/src/disk_commit/process.rs:269`) never selects an
async backend: the `#[cfg(all(target_os = "linux", feature =
"io_uring"))]` and `#[cfg(all(target_os = "windows", feature =
"iocp"))]` arms (`crates/transfer/src/disk_commit/process.rs:277`,
`crates/transfer/src/disk_commit/process.rs:286`) compile out, and
the final return at
`crates/transfer/src/disk_commit/process.rs:295` builds a
`ReusableBufWriter`.

`ReusableBufWriter::write` at
`crates/transfer/src/disk_commit/writer.rs:91` already issues
`write_vectored` for the common "small buffered prefix + large
literal chunk" case at
`crates/transfer/src/disk_commit/writer.rs:97`, so the disk-commit
hot path does collapse two writes into one `writev` syscall. What
it does not do:

- Bypass the unified buffer cache on large transfers (no
  `F_NOCACHE`).
- Overlap consecutive writes with the device. Every `write_vectored`
  blocks the disk-commit thread until the page cache accepts the
  data.
- Batch more than two buffers per syscall. The chunk loop is
  serial.

### 3.3 `MacosWriter` is implemented but unwired

This is the headline finding of this audit and is not reflected in
the earlier `fast-io-macos-fallback.md` / `fast-io-fallback-macos-
vs-linux.md` documents (which both treat `F_NOCACHE` plus `writev`
as "#1657 pending").

The `MacosWriter` type at `crates/fast_io/src/macos_io.rs:58`
implements exactly the F_NOCACHE-plus-writev pattern those audits
called for. It is exported from `crates/fast_io/src/lib.rs:155`. A
workspace-wide search for callers turns up zero hits outside the
module's own tests:

```
$ rg 'MacosWriter|writev_buffers|F_NOCACHE_THRESHOLD' \
    --type rust crates/ \
    | grep -v 'fast_io/src/macos_io.rs' \
    | grep -v 'fast_io/src/lib.rs'
# (no output)
```

`MacosWriter` is therefore dead weight today. The disk-commit
writer at `crates/transfer/src/disk_commit/writer.rs:142` still
constructs `ReusableBufWriter` directly from
`std::fs::File`; nothing branches to `MacosWriter` on macOS.

### 3.4 Whole-file copy: no `posix_fadvise`/`F_NOCACHE` hint (M3 RESOLVED)

When `clonefile` and `fcopyfile` both fail and the chain drops to
`std::fs::copy` (`platform_copy/dispatch.rs:93`), the userspace
buffered copy runs through the unified buffer cache. For tree-wide
copies larger than RAM this evicts hot pages for the rest of the
run.

**Resolved (#2154):** The local-copy executor now applies the macOS
sequential-read advisory at source-file open. The helper lives at
`crates/fast_io/src/macos_io.rs::apply_sequential_read_hint` and is
wired into `crates/engine/src/local_copy/executor/file/copy/transfer/open.rs::open_source_file`.
Files at or above `F_NOCACHE_THRESHOLD` (1 MiB) receive
`fcntl(fd, F_NOCACHE, 1)`, the macOS analogue of Linux's
`posix_fadvise(POSIX_FADV_DONTNEED)`. `F_RDADVISE` is deliberately
not used: it requires a known `(ra_offset, ra_count)` extent and
rsync's delta algorithm seeks unpredictably through the basis file.

### 3.5 Network-to-disk path: no `sendfile`

`crates/fast_io/src/sendfile.rs:147` is gated on
`#[cfg(target_os = "linux")]`. The macOS arm at
`crates/fast_io/src/sendfile.rs:158` falls through to
`copy_via_fd_write` - a `read`/`write` loop. macOS does ship a
`sendfile(2)` (Darwin signature
`sendfile(fd, sockfd, offset, &len, hdrs, flags)`); fast_io does
not call it. The sender-side daemon serving many clients pays a
userspace `memcpy` per byte that the Linux path avoids.

### 3.6 Metadata batching: no statx analogue

`crates/fast_io/src/syscall_batch.rs:132` batches metadata ops, but
the macOS execution at
`crates/fast_io/src/syscall_batch.rs:162`
(`execute_metadata_ops_individual`) is just sequential `metadata` /
`symlink_metadata` calls. Linux io_uring has `IORING_OP_STATX`
which reaps N stats in one ring submission; macOS has no analogue,
and `getattrlistbulk(2)` (the closest equivalent) is not wired in.

## 4. Is the macOS path "good enough"?

The honest read: **partly good, with one easy structural win
available today**.

### 4.1 Wire up `MacosWriter` (low risk, immediate payoff)

The single most concrete action is to add a `#[cfg(target_os =
"macos")]` arm to `make_writer` in
`crates/transfer/src/disk_commit/process.rs:269` that wraps the
file in `MacosWriter::from_file(file, size_hint)`. The hint is
already available from the upstream `total_size` parameter that
threads into `make_writer`. Properties:

- Files >= 1 MiB get `F_NOCACHE` set, eliminating page-cache
  pollution on tree-wide transfers larger than RAM.
- Writes >= 256 KiB are flushed via `writev` covering up to 64
  iovecs per syscall (`MAX_IOV_COUNT = 64` at
  `crates/fast_io/src/macos_io.rs:41`), down from the current
  one-or-two-iovec ceiling in `ReusableBufWriter`.
- No new probe required: `try_set_nocache` already tolerates
  filesystems that reject the flag.
- Sparse mode and append mode (the current `Writer::Buffered`
  callers at `crates/transfer/src/disk_commit/process.rs:279` and
  `:288`) must still use the seek-capable `ReusableBufWriter`.
  `MacosWriter` does not implement `Seek`, so the new arm should
  gate on `!use_sparse && append_offset == 0` exactly like the
  io_uring and IOCP arms above it.

Cost: roughly one screen of code in `disk_commit/process.rs` and a
matching `Writer::Macos { writer: MacosWriter }` variant in
`crates/transfer/src/disk_commit/writer.rs:141`. No new crate, no
new probe, no new CLI flag. The implementation is fully tested
inside `crates/fast_io/src/macos_io.rs` already.

### 4.2 Document the implemented-but-unwired state

Until #1652 is closed by wiring `MacosWriter` in, the existing
audits should be amended (or this one should supersede them) to
note that `F_NOCACHE` plus `writev` was implemented under
`bacb146eb` but has zero callers. Otherwise future planning will
under-count the available wins.

### 4.3 Defer larger surgery

The kqueue backend (#1385) and the macOS `sendfile` arm (section
3.5) remain valid future work. Both require new modules and probes.
Neither is required for the immediate small-file disk-commit win
that section 4.1 delivers.

### 4.4 Where macOS is already healthy

- Local whole-file copy on APFS: `clonefile` is structurally
  optimal, zero data moved.
- Cross-volume and HFS+/SMB whole-file copy: `fcopyfile` is one
  kernel call per file.
- Small files below the 1 MiB threshold: leaving them in the page
  cache is the right call; `F_NOCACHE` would hurt re-read latency.
- `ReusableBufWriter`'s combined-buffer `write_vectored` at
  `crates/transfer/src/disk_commit/writer.rs:97` already collapses
  the "buffered prefix + large literal" case into a single `writev`
  syscall, which is the common shape of delta-token writes.

## 5. Summary

The macOS path is structurally sound for whole-file copies and for
small writes. The two open items are:

1. **Wire `MacosWriter` into the disk-commit dispatcher.** Code
   exists at `crates/fast_io/src/macos_io.rs:58`; callers exist at
   `crates/transfer/src/disk_commit/process.rs:269` and
   `crates/transfer/src/disk_commit/writer.rs:141`. The gap is one
   `Writer::Macos` enum variant and one cfg-gated arm in
   `make_writer`. Closes #1652 cleanly.
2. **Macos `sendfile(2)`** in `crates/fast_io/src/sendfile.rs:158`
   for the sender-side daemon path. Useful but lower priority.

The `io_uring_stub.rs` macOS surface
(`crates/fast_io/src/io_uring_stub.rs:25`-
`crates/fast_io/src/io_uring_stub.rs:984`) is a thorough
type-parity shim, not a performance backend; it routes every call
through `StdFileReader`/`StdFileWriter`. That is the correct shape
for a stub - the gap is not in the stub itself but in the lack of a
macOS-native disk-commit variant beside it.
