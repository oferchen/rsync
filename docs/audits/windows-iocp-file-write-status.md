# Windows IOCP file-write wiring status

Tracking issues: oc-rsync #1868 (IOCP benchmark), #1899 (IOCP vs
`std::fs::File` benchmark), #1900 (IOCP CI matrix). Branch:
`docs/windows-iocp-file-write-audit` (forked from `origin/master`).
Static, source-grounded audit; no benchmarks were collected.

The audit answers one question raised in review: when oc-rsync runs
on Windows with `--features iocp`, does the disk-commit thread submit
file body writes through `IocpDiskBatch` (overlapped `WriteFile` plus
a completion port), or does the dispatcher silently fall back to the
buffered `std::fs::File` path?

Companion documents:

- `docs/audits/windows-iocp-benchmark.md` - measurement plan.
- `docs/audits/windows-iocp-benchmark-plan.md` - parity criteria
  against the Linux io_uring matched workloads in #1868.
- `docs/audits/macos-fastio-fallback.md` - structurally analogous
  question for macOS where the answer is "implemented but unwired"
  (used as the mapping-table template in section 3 below).

## 1. Verdict

**A: IOCP IS wired for file writes in the disk-commit hot path.**

On Windows with the `iocp` cargo feature enabled (which is on by
default in the workspace and in `fast_io`), the disk-commit thread
creates a single `fast_io::IocpDiskBatch` at startup, threads it into
every per-file `process_file` / `process_whole_file` call, and the
per-file `make_writer` dispatcher constructs `Writer::Iocp { batch }`
whenever the file is eligible. `Writer::write_chunk` then routes
delta tokens and whole-file payloads through
`IocpDiskBatch::write_data`, which buffers up to 256 KiB and submits
batched overlapped `WriteFile` calls drained via
`GetQueuedCompletionStatusEx`. The buffered `ReusableBufWriter`
(plain `std::fs::File`) is selected only when the batch could not be
created, when sparse mode is requested, or when the file opens with a
non-zero append offset.

The internal note is correct; the user observation that "file writes
are still standard buffered I/O on Windows" is wrong for any Windows
build that compiles in the `iocp` feature (the default). The only
Windows build that falls back end-to-end is `--no-default-features`
or an explicit `--features` set that omits `iocp`.

## 2. Evidence

1. `crates/transfer/src/disk_commit/writer.rs:141-151` defines the
   `Writer` enum with a dedicated `Iocp { batch: &'a mut
   fast_io::IocpDiskBatch }` variant, gated `#[cfg(all(target_os =
   "windows", feature = "iocp"))]`.
2. `crates/transfer/src/disk_commit/process.rs:286-294` is the
   Windows arm of `make_writer`: when the batch is `Some`, sparse
   mode is off, and `append_offset == 0`, the dispatcher calls
   `batch.begin_file(file)` and returns `Writer::Iocp { batch }`
   without ever wrapping the file in `ReusableBufWriter`.
3. `crates/transfer/src/disk_commit/writer.rs:177-185`
   (`Writer::write_chunk`) routes the `Iocp` arm to
   `batch.write_data(data)`. `:226-244` (`Writer::finish`) commits
   the active file via `batch.commit_file(do_fsync)`, optionally
   issuing `FlushFileBuffers`.
4. `crates/transfer/src/disk_commit/thread.rs:179-203` constructs
   `iocp_batch = try_create_iocp_batch(config.iocp_policy)` once per
   disk-commit thread and passes `iocp_batch.as_mut()` into every
   `process_file` / `process_whole_file` invocation. The batch is
   reused across all files in the transfer, matching the io_uring
   side.
5. `crates/fast_io/src/iocp/disk_batch.rs:50-54` imports `WriteFile`,
   `FlushFileBuffers`, and `FILE_FLAG_OVERLAPPED`, and
   `disk_batch.rs:162-186` (`begin_file`) reopens the caller's `File`
   with `FILE_FLAG_OVERLAPPED` via `ReOpenFile` and associates the
   new handle with the persistent `CompletionPort`. `:196-221`
   (`write_data`) buffers into a reusable 256 KiB scratch buffer and
   spills to `submit_write_batch` (`:310-322`), which issues
   overlapped `WriteFile` calls and drains completions through
   `GetQueuedCompletionStatusEx`.
6. `Cargo.toml:33` includes `"iocp"` in the workspace default
   features, `Cargo.toml:77` defines `iocp = ["transfer/iocp",
   "fast_io/iocp"]`, `crates/transfer/Cargo.toml:90` forwards it to
   `fast_io`, and `crates/fast_io/Cargo.toml:39,55` lists `iocp` in
   the crate default features. Stock `cargo build --target
   x86_64-pc-windows-msvc` therefore compiles in the IOCP path.
7. `crates/transfer/src/disk_commit/config.rs:96,112` sets
   `iocp_policy: fast_io::IocpPolicy::Auto` as the default for
   `DiskCommitConfig`, so a stock client invocation lands in the
   `Auto` branch of `try_create_iocp_batch`
   (`thread.rs:100-107`), which calls
   `IocpDiskBatch::try_new(&IocpConfig::default())`.

## 3. Linux / macOS / Windows mapping for the disk-commit `Writer`

| Platform / feature set | Eligible files (no sparse, `append_offset == 0`) | Sparse mode | Append mode (`append_offset > 0`) | Source |
|---|---|---|---|---|
| Linux, `--features io_uring` (default) | `Writer::IoUring { batch: &mut IoUringDiskBatch }` | `Writer::Buffered(ReusableBufWriter)` | `Writer::Buffered(ReusableBufWriter)` | `crates/transfer/src/disk_commit/process.rs:277-285` |
| Linux, `--no-default-features` (no io_uring) | `Writer::Buffered(ReusableBufWriter)` (cfg arm compiled out) | `Writer::Buffered` | `Writer::Buffered` | `crates/transfer/src/disk_commit/process.rs:277` cfg gate |
| macOS, default features | `Writer::Buffered(ReusableBufWriter)` | `Writer::Buffered` | `Writer::Buffered` | `crates/transfer/src/disk_commit/process.rs:295`; `MacosWriter` exists at `crates/fast_io/src/macos_io.rs:58` but is not wired into `make_writer` (see `docs/audits/macos-fastio-fallback.md` section 3.3) |
| Windows, `--features iocp` (default) | `Writer::Iocp { batch: &mut IocpDiskBatch }` | `Writer::Buffered(ReusableBufWriter)` | `Writer::Buffered(ReusableBufWriter)` | `crates/transfer/src/disk_commit/process.rs:286-294`; `crates/transfer/src/disk_commit/writer.rs:147-151` |
| Windows, `--no-default-features` (no iocp) | `Writer::Buffered(ReusableBufWriter)` (cfg arm compiled out) | `Writer::Buffered` | `Writer::Buffered` | `crates/transfer/src/disk_commit/process.rs:286` cfg gate; `IocpDiskBatch` resolves to the stub in `crates/fast_io/src/iocp_stub.rs:134-148` whose `try_new` returns `None` |
| Windows, `iocp` feature on but `--no-iocp` (policy `Disabled`) | `Writer::Buffered(ReusableBufWriter)` | `Writer::Buffered` | `Writer::Buffered` | `crates/transfer/src/disk_commit/thread.rs:101-107` returns `None` for `IocpPolicy::Disabled`; `make_writer` then falls through to `Writer::Buffered` at `process.rs:295` |
| Windows, `iocp` feature on but `IocpDiskBatch::try_new` returns `None` (`is_iocp_available()` false, port creation fails) | `Writer::Buffered(ReusableBufWriter)` | `Writer::Buffered` | `Writer::Buffered` | `crates/fast_io/src/iocp/disk_batch.rs:143-148`; status logged at `crates/transfer/src/disk_commit/thread.rs:146-164` |

Notes:

- Sparse mode and append mode require `Seek`, which neither
  `IoUringDiskBatch` nor `IocpDiskBatch` provides. The fallback to
  `ReusableBufWriter` for those two cases is intentional and mirrors
  upstream rsync's behaviour: sparse delta-token writes punch holes
  via `seek(SeekFrom::Current(n))` against the underlying `File`
  (see `crates/transfer/src/disk_commit/writer.rs:124-129` and
  `crates/transfer/src/disk_commit/process.rs:256-264`).
- The `IocpDiskBatch` path does not apply the
  `IOCP_MIN_FILE_SIZE = 64 KB` threshold that the alternate
  per-file `IocpWriterFactory` in `crates/fast_io/src/iocp/file_factory.rs:196,452`
  consults. That threshold guards the standalone
  `writer_from_file` factory only. The disk-commit thread submits
  every eligible file - regardless of size - through one shared
  batch and one shared completion port, amortising setup across the
  whole transfer.
- `io_uring` and IOCP are mutually exclusive at run time even though
  both arms compile on their respective platforms: the disk-commit
  thread only attempts `try_create_iocp_batch` when
  `try_create_disk_batch` returned `None`
  (`crates/transfer/src/disk_commit/thread.rs:179-187`). In
  practice the two backends are also mutually exclusive by target
  triple.

## 4. Conditions under which IOCP is skipped

For a Windows build with the `iocp` feature compiled in, the disk
batch is bypassed only by these conditions (all of which are visible
to the user via `-vv` logging at
`crates/transfer/src/disk_commit/thread.rs:146-164`):

1. **Policy says no.** `DiskCommitConfig.iocp_policy ==
   IocpPolicy::Disabled` (`crates/transfer/src/disk_commit/thread.rs:101-103`).
   This is the explicit user opt-out.
2. **Runtime probe failed.** `is_iocp_available()` returns `false`
   from the cached probe in
   `crates/fast_io/src/iocp/config.rs:91-97`, or `CreateIoCompletionPort`
   failed during `IocpDiskBatch::new`. Either path causes
   `try_create_iocp_batch` to return `None`.
3. **Sparse mode (`--sparse`).** Selected at
   `crates/transfer/src/disk_commit/process.rs:288` (the
   `!use_sparse` guard). Sparse writes require `Seek`.
4. **Append mode (`--append`, `--append-verify`).** Selected at
   `crates/transfer/src/disk_commit/process.rs:288` (the
   `append_offset == 0` guard). Append-mode files are opened and
   seeked past the existing prefix at
   `crates/transfer/src/disk_commit/process.rs:240-243`; the batch
   writer would otherwise overwrite that prefix because it tracks
   its own absolute offset.

No size-based gate fires inside `make_writer` itself. The headline
is: on a default Windows build, every non-sparse, non-append file
written by the receiver thread goes through IOCP.

## 5. Why the user observation may have arisen

Three plausible sources of the misreading, listed because they
recur in IOCP review threads:

- **Socket vs file confusion.** `crates/fast_io/src/iocp/socket.rs`
  is the IOCP socket helper used by the network transport, and the
  `pump.rs` module drains both file and socket completions. A
  scan that stops at `socket.rs` and `pump.rs` will conclude that
  IOCP is only wired for sockets and miss `disk_batch.rs` entirely.
  The disk-commit dispatcher reaches IOCP through `disk_batch.rs`,
  not through `socket.rs`.
- **Stub on the wrong platform.** Reading the workspace on macOS or
  Linux resolves `fast_io::IocpDiskBatch` to the stub at
  `crates/fast_io/src/iocp_stub.rs:134-195`, where every method
  returns `Unsupported`. The real implementation only compiles on
  `cfg(all(target_os = "windows", feature = "iocp"))`.
- **`--features iocp` already on by default.** A reader who expects
  an explicit feature flag at the call site may miss that the
  workspace `Cargo.toml:33` lists `iocp` among the default
  features, so a stock `cargo build` on Windows already activates
  the path.

## 6. Related tasks

- **#1868** - IOCP benchmark coverage across the same workloads as
  the Linux io_uring matched-workload set. The verdict in section 1
  unblocks #1868: there is a real IOCP file-write path to bench
  against the Linux io_uring numbers. Benchmark methodology is laid
  out in `docs/audits/windows-iocp-benchmark.md`.
- **#1899** - IOCP vs `std::fs::File` head-to-head benchmark. The
  natural comparison is `--features iocp` (the current default) vs
  `--no-default-features --features <rest minus iocp>` or
  `--iocp=disabled` at run time, both of which land in the
  `Writer::Buffered` arm at
  `crates/transfer/src/disk_commit/process.rs:295`. Both endpoints
  exist in tree today.
- **#1900** - IOCP CI matrix. The CI job needs to cover at least:
  `--features iocp` on `x86_64-pc-windows-msvc` (the production
  path), `--no-default-features` on Windows (the
  `Writer::Buffered`-only fallback used by the cross-platform
  matrix), and non-Windows targets where the cfg gate compiles the
  IOCP arm out entirely (regression check on the stub at
  `crates/fast_io/src/iocp_stub.rs:134-211`). Open hardening tasks
  for the IOCP path itself remain in flight under #1897, #1898,
  #1929, and #1930; the file-write wiring is complete.

## 7. Summary

The disk-commit dispatcher on Windows builds with the default
`iocp` feature constructs `Writer::Iocp { batch: &mut
IocpDiskBatch }` for every non-sparse, non-append output file and
routes its writes through batched overlapped `WriteFile` calls
drained by `GetQueuedCompletionStatusEx`. The only end-to-end
fallback to `std::fs::File` on Windows happens under three explicit
conditions (policy `Disabled`, sparse mode, append mode) or when
the runtime probe rejects IOCP. The cfg gates at
`crates/transfer/src/disk_commit/writer.rs:147-151` and
`crates/transfer/src/disk_commit/process.rs:286-294`, together with
the default feature set in `Cargo.toml:33,77`, make IOCP the
production path on stock Windows builds.
