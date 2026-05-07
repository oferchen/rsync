# macOS fast_io fallback paths vs Linux io_uring (#1652)

Tracking issue: oc-rsync #1652. Static, source-grounded audit of the
macOS fallback chain in `crates/fast_io/`. No runtime traces or
benchmarks were collected; quantitative claims are derived from syscall
semantics and source behaviour. Companion docs (referenced, not
duplicated):

- `docs/audits/fastio-macos-fallback.md` - earlier macOS source walk.
- `docs/audits/fast-io-fallback-macos-vs-linux.md` - extended per-syscall
  budget for the same gap.
- `docs/audits/macos-dispatch-io.md` - #1653 dispatch_io evaluation.
- `docs/audits/async-file-writer-trait.md` - #1657 trait blueprint.

## 1. Current macOS fast_io paths

Whole-file copy dispatch lives in
`crates/fast_io/src/platform_copy/dispatch.rs`:

- `platform_copy_impl` (line 62, gated `#[cfg(target_os = "macos")]`)
  selects, in order: `clonefile_impl` -> `fcopyfile_impl` ->
  `std::fs::copy`.
- `clonefile_impl` (line 151) wraps `libc::clonefile(src, dst, 0)` for
  APFS copy-on-write. Reports `CopyResult::new(0, CopyMethod::Clonefile)`
  on success since no data is copied.
- `fcopyfile_impl` (line 186) opens both fds and calls
  `libc::fcopyfile(src_fd, dst_fd, NULL, COPYFILE_DATA)` for kernel-side
  data copy on HFS+, NFS, SMB, or APFS cross-device targets.
- The `std::fs::copy` arm is the portable `read`/`write` userspace loop;
  on macOS it does not fan out to `copyfile(3)` automatically.

Public re-exports (`platform_copy/mod.rs:222`, `:249`) expose
`try_clonefile` and `try_fcopyfile` for callers that want to skip the
dispatch chain. `NoCowPlatformCopy` (line 106) forces `std::fs::copy`
when `--no-cow` is set, bypassing every fast path.

No macOS code uses `dispatch_io`, `aio_*`, `F_NOCACHE`, or any
async-completion surface today. The `fast_io::traits::FileWriter` impls
on macOS resolve to `Writer::Buffered` only (verified in
`crates/transfer/src/disk_commit/writer.rs`); there is no
`Writer::DispatchIo` variant.

## 2. Gap vs Linux io_uring

Linux ships an asynchronous submission/completion path:

- `crates/fast_io/src/io_uring/` provides batched submit, registered
  buffers, and a per-session ring under
  `#[cfg(all(target_os = "linux", feature = "io_uring"))]`.
- `Writer::IoUring { batch }` (`disk_commit/writer.rs`) feeds the
  receiver hot path so multi-chunk writes coalesce into one syscall.

macOS has no equivalent. Every `fast_io` call is synchronous from the
caller's perspective. `clonefile`/`fcopyfile` complete in the kernel in
one call, but the receiver chunk-write loop, file-open burst, and
metadata fan-out remain blocking. `dispatch_io` (libdispatch) exists on
the platform and could deliver async submit/completion semantics
analogous to io_uring but is not wired in (#1653 evaluated and
deferred).

## 3. Fallback chain in the transfer pipeline

Whole-file copy callers reach the dispatch chain via the `PlatformCopy`
trait:

- `crates/engine/src/local_copy/clonefile.rs:79` invokes
  `platform_copy.copy_file(src, dst, size_hint)` from the local-copy
  executor (`local_copy/transfer/execute.rs:170`).
- `crates/engine/src/local_copy/win_copy.rs:132` is the Windows-only
  callsite using the same trait.
- The trait default (`builder/definition.rs:277`) is
  `Arc::new(DefaultPlatformCopy::new())`, threaded through
  `CoreConfig` -> `LocalCopyOptions` (`options/platform_copy.rs`).

Effective macOS chain per local copy: `clonefile` (zero-data, CoW) ->
`fcopyfile` (single-syscall kernel copy) -> `std::fs::copy` (userspace
loop). All three are synchronous; only the third allocates a userspace
buffer. Network transfers do not exercise this dispatch; they go through
`disk_commit::writer::Writer::Buffered`.

## 4. Suspected bottleneck

Small-file directory copies pay full per-file syscall overhead:

- `clonefile` is invoked per entry: `open` is implicit but the kernel
  still walks both path components, allocates inodes, and journals the
  clone. On APFS this is ~30-60 us per file (inferred from APFS
  benchmarks in the companion audit).
- When `clonefile` rejects (HFS+, cross-device, dst exists), the chain
  drops to `fcopyfile`, which itself opens both files (`open` x2),
  copies, and closes - roughly 4 syscalls per file plus the data move.
- The `std::fs::copy` final arm is `open` + repeated `read`/`write` +
  `close`: at minimum 4 syscalls plus N data syscalls per file, with no
  batching.
- No `posix_fadvise` / `F_NOCACHE` hint is set, so streaming a tree
  larger than the unified buffer cache pollutes warm pages and forces
  reclaim work for the rest of the run.

A directory of N small files therefore costs O(N) blocking round-trips
to the kernel with no opportunity for overlap, versus the io_uring
build's batched submit on Linux.

## 5. Improvement candidates

- **`dispatch_io` batched I/O (#1653, evaluated).** Wraps an fd as a
  `dispatch_io_t` channel; `dispatch_io_read`/`_write` complete on a
  GCD queue. Closes the async-submit gap with no kernel changes. Audit
  recommended deferring until #1657 lands; revisit once the trait is in.
- **`F_NOCACHE` for streaming (#1657, pending).** Set
  `fcntl(fd, F_NOCACHE, 1)` on destination fds during large transfers
  to bypass the unified buffer cache. Documented in
  `async-file-writer-trait.md:266`. Cheap, no API change.
- **`AsyncFileWriter` trait macOS impl (#1657).** Adds an
  `AppleAsyncFileWriter` that pairs `writev(2)` with `F_NOCACHE` on
  large writes and routes small writes through `Writer::Buffered`. This
  is the single piece of work that gives macOS a parity surface to
  io_uring on Linux and IOCP on Windows.
- **`fcopyfile` with `COPYFILE_CLONE_FORCE` for known APFS targets.**
  Skip the `clonefile` -> `fcopyfile` two-step when the destination is
  proven to be on the same APFS volume; the kernel already picks the
  CoW path internally. Removes one failed syscall per cross-volume
  copy.

Tracking: #1652 (this audit), #1653 (dispatch_io), #1657 (writer
trait + F_NOCACHE), #1385 (kqueue receiver backend).
