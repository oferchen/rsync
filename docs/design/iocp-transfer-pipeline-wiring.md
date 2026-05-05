# IOCP wiring for the transfer disk-commit pipeline (#1868)

This note captures the design that closes #1868: making the Windows
disk-commit thread consume the IOCP infrastructure already in
`crates/fast_io/src/iocp/`. The Linux side has consumed
`fast_io::IoUringDiskBatch` since #1086; until #1868 the Windows side
fell back to synchronous `std::fs::File` writes regardless of whether
the `iocp` feature was compiled in. This doc covers the symmetric
batched dispatch surface, the runtime probe, the backpressure model,
the test surface, and the ordered task list. No wire-protocol changes;
no changes to upstream-compatible behaviour.

## 1. Problem statement

Disk commit on the receiver runs on a dedicated thread that consumes
`FileMessage` items from a bounded SPSC channel
(`crates/transfer/src/disk_commit/thread.rs:47`,
`spawn_disk_thread`). Inside the disk thread, every per-file write is
dispatched through `make_writer`
(`crates/transfer/src/disk_commit/process.rs:269`) which constructs a
`Writer` variant for the file's lifetime. Until #1868 the function had a
single batched branch:

```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
{
    if !use_sparse && append_offset == 0 {
        if let Some(batch) = disk_batch {
            batch.begin_file(file)?;
            return Ok(Writer::IoUring { batch });
        }
    }
}
Ok(Writer::Buffered(ReusableBufWriter::new(file, write_buf)))
```

Every Windows build hit the buffered fall-through. The buffered writer
serialises every chunk through `std::fs::File::write_all`
(`crates/transfer/src/disk_commit/writer.rs:99`), so the disk thread
issued exactly one synchronous `WriteFile` syscall per buffered flush -
no overlapped I/O, no completion port, no batching. Windows builds were
"compiled in, available" for the IOCP factory types
(`crates/fast_io/src/lib.rs:188`-`199`,
`iocp_status_detail_impl`) yet none of that machinery touched the
write path that actually moves transfer bytes onto disk.

Concretely, on a `windows-msvc` build with default features
(`iocp` is in the default set, see `Cargo.toml:33`) the disk thread:

- Never opened a file with `FILE_FLAG_OVERLAPPED`.
- Never associated a handle with an `HCOMPLETIONPORT`.
- Never submitted overlapped `WriteFile` calls.
- Never called `GetQueuedCompletionStatusEx`.

The fix landed via the symmetric branch now visible at
`crates/transfer/src/disk_commit/process.rs:286-294`:

```rust
#[cfg(all(target_os = "windows", feature = "iocp"))]
{
    if !use_sparse && append_offset == 0 {
        if let Some(batch) = iocp_batch {
            batch.begin_file(file)?;
            return Ok(Writer::Iocp { batch });
        }
    }
}
```

The `Writer` enum carries the new variant at
`crates/transfer/src/disk_commit/writer.rs:147-150`, and dispatch
through `write_chunk`, `flush_and_sync`, and `finish` is mirrored
linearly with the io_uring arm
(`writer.rs:182-184`, `207-209`, `236-242`). The disk thread itself
constructs an `IocpDiskBatch` via
`try_create_iocp_batch`
(`crates/transfer/src/disk_commit/thread.rs:92`) and threads it into
`process_file` / `process_whole_file` alongside the existing io_uring
batch handle (`thread.rs:188-209`).

This document records the evidence behind that wiring, the design
choices that produced it, and the open work needed to get from
"wired" to "shipped on the Windows CI matrix with regression coverage".

## 2. Inventory of existing IOCP infrastructure

Every piece below lives behind `#[cfg(all(target_os = "windows",
feature = "iocp"))]`. The non-Windows stub (`crates/fast_io/src/iocp_stub.rs`)
provides the same public surface returning `Unsupported` errors so the
crate compiles on Linux and macOS.

### 2.1 Module map

`crates/fast_io/src/iocp/mod.rs:28-37` declares the submodules.
`crates/fast_io/src/iocp/mod.rs:39-53` re-exports the public surface.

| File | LoC | Role |
|------|----:|------|
| `crates/fast_io/src/iocp/completion_port.rs` | 109 | RAII wrapper for a Windows `HCOMPLETIONPORT`; `CompletionPort::new` and `associate` (lines 27, 47). |
| `crates/fast_io/src/iocp/config.rs` | 204 | `IocpConfig`, `is_iocp_available` runtime probe, `iocp_availability_reason` log string (line 91, 113). |
| `crates/fast_io/src/iocp/disk_batch.rs` | 988 | `IocpDiskBatch`: batched overlapped writer that owns one completion port reused across files (line 87). |
| `crates/fast_io/src/iocp/error.rs` | 165 | Typed `IocpError` for `ERROR_INVALID_PARAMETER` (#1929) and `ERROR_INSUFFICIENT_BUFFER` (#1930). |
| `crates/fast_io/src/iocp/file_factory.rs` | 665 | `IocpOrStdReader`, `IocpOrStdWriter`, path-and-handle-based factories with `IocpPolicy` fallback. |
| `crates/fast_io/src/iocp/file_reader.rs` | 409 | `IocpReader`: per-file overlapped reader for one-shot use. |
| `crates/fast_io/src/iocp/file_writer.rs` | 454 | `IocpWriter`: per-file overlapped writer; `create_for_append` (line 54) reopens an existing handle. |
| `crates/fast_io/src/iocp/overlapped.rs` | 152 | `OverlappedOp`: pinned `OVERLAPPED` + co-located buffer; `as_overlapped_ptr` returns the stable pointer (line 57). |
| `crates/fast_io/src/iocp/pump.rs` | 781 | `CompletionPump`: shared port with a worker thread that fans completions to per-op handlers. |
| `crates/fast_io/src/iocp/socket.rs` | 628 | Overlapped TCP socket reader/writer (out of scope for disk-commit). |

### 2.2 Lifecycle of a disk-commit batch

The end-to-end happy path for a single file inside the disk thread is:

1. `try_create_iocp_batch`
   (`crates/transfer/src/disk_commit/thread.rs:92`) probes
   `is_iocp_available` and constructs an `IocpDiskBatch` once, reused
   across every file the thread processes.
2. `IocpDiskBatch::new`
   (`crates/fast_io/src/iocp/disk_batch.rs:123`) creates the shared
   `CompletionPort` with one worker concurrency
   (`completion_port.rs:27`).
3. `IocpDiskBatch::begin_file`
   (`disk_batch.rs:163`) reopens the caller's `File` with
   `FILE_FLAG_OVERLAPPED` via `ReOpenFile`
   (`disk_batch.rs:363`) and associates the new handle with the port
   under a per-file completion key (`disk_batch.rs:172`).
4. `IocpDiskBatch::write_data`
   (`disk_batch.rs:197`) buffers writes up to the configured
   `buffer_size` (default 256 KB matching `wf_writeBufSize`,
   `disk_batch.rs:62`) and flushes batches via `submit_write_batch`
   (`disk_batch.rs:406`) which keeps `concurrent_ops` (default 4,
   `config.rs:26`) overlapped writes in flight at any time.
5. Completions are reaped through `GetQueuedCompletionStatusEx` with a
   batch of `COMPLETION_DRAIN_BATCH = 64` entries
   (`disk_batch.rs:556`), matching the io_uring CQE drain granularity.
6. `IocpDiskBatch::commit_file`
   (`disk_batch.rs:244`) flushes any buffered data, optionally calls
   `FlushFileBuffers` for `--fsync`-style semantics
   (`disk_batch.rs:263`), closes the overlapped handle, and returns
   the original `File` and `bytes_written` so the disk-commit thread
   can rename or truncate.

### 2.3 Status of in-flight tasks

| Task | Status | Reference |
|------|--------|-----------|
| #1717-#1721 | merged | `OverlappedOp` pinning + offset bookkeeping (`overlapped.rs:16-107`) |
| #1821 | merged | Per-platform batch dispatch threading (`thread.rs:171-209`, `process.rs:38-39`) |
| #1928 | merged | Overlapped TCP socket layer (`iocp/socket.rs`); not consumed by disk-commit |
| #1897 | in-flight | Symmetric `IocpDiskBatch` surface parity polish |
| #1898 | in-flight | Pump-based dispatch surface (`pump.rs:1-78`) for layered batching |
| #1929 | in-flight | `writer_from_file` reopen-with-overlapped flow (`file_writer.rs:54`, `error.rs:36-47`) |
| #1930 | in-flight | `ERROR_INSUFFICIENT_BUFFER` retry growth (`error.rs:49-65`, `pump.rs:346-389`) |
| #1931 | open | Partial-write resubmission tests for `submit_write_batch` |
| #1932 | open | Disk-full / `ERROR_DISK_FULL` simulation for the disk-commit path |
| #1900 | open | Wire `windows-latest` into the IOCP-enabled CI matrix |
| #1899 | open | Doc-and-status work referenced by `iocp_status_detail` |
| #1871 | open | High-concurrency stress harness for the IOCP write path |

## 3. Symmetric design

### 3.1 Why an enum, not a trait

The original sketch in this issue floated an `IoBackend` trait abstracting
io_uring and IOCP behind a single Rust trait object. The repository
already chose a different pattern: cfg-gated concrete types injected
through enum variants. Three reasons make that pattern the right fit and
trump introducing a trait:

- The two backends are mutually exclusive at the platform level. A
  Windows build never sees `IoUringDiskBatch`; a Linux build never sees
  `IocpDiskBatch`. A trait would force every call site to carry a
  `Box<dyn DiskBatch>` (or generic over `B: DiskBatch`) for no benefit;
  the stub modules already give us "compile everywhere" portability.
- The `Writer` variants in `crates/transfer/src/disk_commit/writer.rs`
  capture lifetime relationships that a trait cannot express cleanly.
  Each batch arm holds `&'a mut fast_io::IocpDiskBatch`
  (`writer.rs:148-150`) borrowed from the disk-thread-local batch; the
  Buffered arm owns its `ReusableBufWriter`. A `dyn DiskBatch` would
  collapse those lifetimes.
- The buffered fall-through is a real third arm of the enum. It is not
  a trait impl - it is a different shape (`Write + Seek` for sparse
  mode) backed by a thread-local 256 KB buffer that the batched arms do
  not need. Modelling that as a trait would require `Self: Seek + Write
  + ...` on the trait or a parallel `BufferedWriter` shadow type.

The chosen design therefore mirrors the io_uring path: each backend is a
concrete `IocpDiskBatch` / `IoUringDiskBatch`; cfg-gated `Writer` enum
variants dispatch per-file; the disk thread holds one optional handle of
each kind so the runtime probe decides which arm gets used.

### 3.2 Surface parity

Both batch types share the following shape so the dispatch code paths
in `process.rs` and `writer.rs` differ only in the cfg gate and the
type name. The methods, signatures, and error contracts are intentionally
identical, established in #1821 and policed by the symmetric
implementations:

| Method | io_uring | IOCP |
|--------|---------|------|
| `new(&Config) -> io::Result<Self>` | `disk_batch.rs:70` | `disk_batch.rs:123` |
| `try_new(&Config) -> Option<Self>` | `disk_batch.rs:87` | `disk_batch.rs:144` |
| `begin_file(File) -> io::Result<()>` | `disk_batch.rs:103` | `disk_batch.rs:163` |
| `write_data(&[u8]) -> io::Result<()>` | `disk_batch.rs:126` | `disk_batch.rs:197` |
| `flush(&mut self) -> io::Result<()>` | `disk_batch.rs:158` | `disk_batch.rs:230` |
| `commit_file(do_fsync: bool) -> io::Result<(File, u64)>` | `disk_batch.rs:170` | `disk_batch.rs:244` |
| `bytes_written` / `bytes_written_with_pending` | `disk_batch.rs:194,200` | `disk_batch.rs:280,286` |

Both types are explicitly `!Send` and `!Sync`. The disk-commit thread
is single-threaded by construction, and both backends amortise their
per-op state across one owner. This mirrors the documented thread-safety
contract on each (`io_uring/disk_batch.rs:42-44`,
`iocp/disk_batch.rs:84-86`).

### 3.3 Drain semantics

Both backends batch up to 64 completions per drain syscall (io_uring
CQEs vs `COMPLETION_DRAIN_BATCH = 64` at
`iocp/disk_batch.rs:68`). The IOCP drain blocks with
`DRAIN_TIMEOUT_MS = u32::MAX` (`disk_batch.rs:73`) because the batch
knows exactly how many submissions it must reap. Spurious
`WAIT_TIMEOUT` returns retry (`disk_batch.rs:578-580`). Short writes
reschedule the unwritten tail at the proper offset
(`disk_batch.rs:445-484`); zero-byte completions surface as
`ErrorKind::WriteZero` (`disk_batch.rs:473-477`).

## 4. Dispatch wiring plan

This section documents the shape of the wiring as it exists now in
`crates/transfer/src/disk_commit/`. Each change below was applied
symmetrically next to its io_uring counterpart so the cfg branches are
visually paired and easy to keep in lockstep.

### 4.1 `disk_commit/thread.rs`

Before #1868, the disk thread carried a single batch handle:

```rust
// before
fn disk_thread_main(...) {
    let mut write_buf = Vec::with_capacity(WRITE_BUF_SIZE);
    let mut disk_batch = try_create_disk_batch(config.io_uring_policy);
    log_io_uring_status(config.io_uring_policy, disk_batch.is_some());
    while let Ok(msg) = file_rx.recv() {
        match msg {
            FileMessage::Begin(begin) => {
                let result = process_file(
                    &file_rx, &buf_return_tx, &config, *begin,
                    &mut write_buf, disk_batch.as_mut(),
                );
                // ...
```

After (`thread.rs:170-225`):

```rust
// after
fn disk_thread_main(...) {
    let mut write_buf = Vec::with_capacity(WRITE_BUF_SIZE);
    let mut disk_batch = try_create_disk_batch(config.io_uring_policy);
    // io_uring takes precedence on Linux; only attempt IOCP if io_uring
    // is not active. In practice the two backends are mutually
    // exclusive by platform, but this keeps the invariant explicit.
    let mut iocp_batch = if disk_batch.is_none() {
        try_create_iocp_batch(config.iocp_policy)
    } else {
        None
    };
    log_io_uring_status(config.io_uring_policy, disk_batch.is_some());
    log_iocp_status(config.iocp_policy, iocp_batch.is_some());

    while let Ok(msg) = file_rx.recv() {
        match msg {
            FileMessage::Begin(begin) => {
                let result = process_file(
                    &file_rx, &buf_return_tx, &config, *begin,
                    &mut write_buf,
                    disk_batch.as_mut(),
                    iocp_batch.as_mut(),
                );
                // ...
```

`try_create_iocp_batch` (`thread.rs:92-99`) and `log_iocp_status`
(`thread.rs:138-156`) are direct mirrors of the io_uring helpers
(`thread.rs:71-84`, `106-133`). The cfg-gated mutual exclusion at
`thread.rs:175-179` keeps the type-level "at most one batch" invariant
that `process.rs` relies on.

### 4.2 `disk_commit/process.rs`

`process_file` and `process_whole_file` gained the `iocp_batch:
Option<&mut fast_io::IocpDiskBatch>` argument
(`process.rs:39`, `process.rs:156`) and forward it to `make_writer`
(`process.rs:47`, `process.rs:164`). The dispatch in `make_writer` is the
single load-bearing change:

```rust
// crates/transfer/src/disk_commit/process.rs:268-296
#[allow(unused_variables)] // batch params are unused on platforms without their backend
fn make_writer<'a>(
    file: fs::File,
    write_buf: &'a mut Vec<u8>,
    disk_batch: Option<&'a mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&'a mut fast_io::IocpDiskBatch>,
    use_sparse: bool,
    append_offset: u64,
) -> io::Result<Writer<'a>> {
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    {
        if !use_sparse && append_offset == 0 {
            if let Some(batch) = disk_batch {
                batch.begin_file(file)?;
                return Ok(Writer::IoUring { batch });
            }
        }
    }
    #[cfg(all(target_os = "windows", feature = "iocp"))]
    {
        if !use_sparse && append_offset == 0 {
            if let Some(batch) = iocp_batch {
                batch.begin_file(file)?;
                return Ok(Writer::Iocp { batch });
            }
        }
    }
    Ok(Writer::Buffered(ReusableBufWriter::new(file, write_buf)))
}
```

The same gating rules apply on both arms: sparse mode requires `Seek`
which neither batch provides; append mode opens the destination in place
and seeks past existing content, while the batch writers issue
absolute-offset submissions starting at 0. Both branches therefore
defer to the buffered writer when sparse or appending.

### 4.3 `disk_commit/writer.rs`

`Writer` carries the new variant at `writer.rs:147-150`. Dispatch is
mirrored linearly across the four touch points:

- `buffered_for_sparse` (`writer.rs:160-174`) - both batched arms
  `unreachable!` because the caller filtered them out in `make_writer`.
- `write_chunk` (`writer.rs:177-185`) - delegates to
  `IocpDiskBatch::write_data`.
- `flush_and_sync` (`writer.rs:192-210`) - returns `Ok(())` for both
  batched arms; flush + fsync are folded into `commit_file`.
- `finish` (`writer.rs:226-244`) - calls `IocpDiskBatch::commit_file`,
  optionally fsyncing via `FlushFileBuffers`.

## 5. Feature flag and runtime probe

The dispatch is gated by the cargo feature `iocp` and the runtime probe
`fast_io::is_iocp_available()`:

- `iocp` is a default feature
  (`Cargo.toml:33`, `crates/fast_io/Cargo.toml:39`) and compiles the
  real module only on Windows
  (`crates/fast_io/src/lib.rs:124-128`). Disabling it routes through
  the stub in `iocp_stub.rs`, where `IocpDiskBatch::try_new` returns
  `None` on every platform.
- `is_iocp_available()`
  (`crates/fast_io/src/iocp/config.rs:91`) caches a probe result in
  `IOCP_STATUS: AtomicU8`. The probe creates a one-off port via
  `CreateIoCompletionPort(INVALID_HANDLE_VALUE, ...)`
  (`config.rs:127-141`) and closes it. Vista+ always succeeds; legacy
  hosts or sandboxed environments fall through to buffered writes.

This mirrors the io_uring probe (#1740) which caches in the same shape
(`crates/fast_io/src/lib.rs:349`); both flow through `try_create_*_batch`
(`thread.rs:71-99`).

`IocpPolicy` (`crates/fast_io/src/lib.rs:436-453`) provides three
states: `Auto` (default; probe and fall back silently), `Enabled`
(prefer IOCP; the disk thread does not abort on probe failure since it
cannot fail the transfer unilaterally - operators that need a hard
guarantee call `iocp_availability_reason()` up-front), and `Disabled`
(bypass the probe entirely). `DiskCommitConfig::iocp_policy`
(`crates/transfer/src/disk_commit/config.rs:81-89`) carries the value
without touching CLI surfaces. CLI flags (`--iocp` / `--no-iocp`,
analogues to `--io-uring` / `--no-io-uring`) are tracked separately
under #1899.

## 6. Backpressure and ordering

`BoundedReorderBuffer`
(`crates/transfer/src/reorder_buffer.rs:55-64`) enforces wire-arrival
order across pipelined files. It operates on per-file sequence numbers
assigned at dispatch time, drains in order, and feeds the disk thread
one `FileMessage` at a time. The disk thread is single-consumer and
processes every message strictly in arrival order
(`thread.rs:184-225`); the IOCP write path inherits the same
in-order-commit guarantee as the io_uring path.

Inside one file, `IocpDiskBatch` keeps `concurrent_ops` overlapped
writes in flight (default 4, `config.rs:26-30`). Out-of-order
completions among those submissions are reconciled at the OVERLAPPED
pointer level (`disk_batch.rs:445-471`): each in-flight op holds its
absolute file offset (`overlapped.rs:99-107`,
`disk_batch.rs:494-505`) and the drain matches completed entries to
in-flight ops by `OVERLAPPED *` address - the OVERLAPPED address is
the per-op identity, replacing io_uring's `OpTag`/`user_data`.

Two ordering details unique to IOCP:

- Synchronous-success completions still post to the port because we
  never set `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`
  (`disk_batch.rs:507-510`); the drain reaps them uniformly with
  `ERROR_IO_PENDING` cases. Skipping the port on success would break
  the invariant that in-flight count equals outstanding completions.
- `FILE_SKIP_SET_EVENT_ON_HANDLE` (`config.rs:99-108`, `lib.rs:194`)
  is enabled where available since we wait on the port, never on the
  file handle's event. Legacy hosts return `false` and the kernel
  signals the unused event harmlessly.

No additions to the reorder buffer are needed. The window-based buffer
operates at file granularity; both backends sit inside one file's slot,
and the disk thread processes slots in order.

## 7. Test strategy

The wiring lands on Windows runners only when the regression surface
covers the failure modes the IOCP path can produce. Each task below has
a target file and an explicit assertion shape.

- **#1931 Partial-write resubmission**: targeted unit tests in
  `crates/fast_io/src/iocp/disk_batch.rs::tests` that drive
  `submit_write_batch` against a file that simulates a short write.
  Validate that `total_written` matches `data.len()`, that the
  resubmission walks the unwritten tail at the correct offset
  (`disk_batch.rs:480-484`), and that a zero-byte completion produces
  `ErrorKind::WriteZero` (`disk_batch.rs:473-477`).
- **#1932 Disk-full simulation**: integration test in
  `crates/transfer/tests/` that pre-fills a small ramdisk-backed temp
  directory and runs a multi-file transfer against the disk-commit
  thread. Expect `ERROR_DISK_FULL` to surface as `io::ErrorKind::Other`
  with a context-tagged message and the disk thread to abort cleanly
  rather than wedging on the unreaped completion port.
- **#1871 High-concurrency stress**: load test in
  `crates/transfer/tests/disk_commit_stress.rs` that drives 1000+ small
  files (< 64 KB) and 100+ medium files (1-16 MB) through
  `spawn_disk_thread` with `IocpPolicy::Enabled`. Assertions: every
  committed file matches its expected payload digest, the in-flight
  completion port is drained at exit, and `IocpDiskBatch::Drop`
  finalises without leaking handles.
- **#1900 CI matrix**: add a `windows-latest` job to `.github/workflows/ci.yml`
  that runs `cargo nextest run --workspace --all-features` against the
  full disk-commit test suite. The existing Windows job at `ci.yml:167`
  already builds with default features (which include `iocp`); the new
  job runs nextest including the IOCP-only tests.

The existing in-tree coverage to extend:

- `crates/fast_io/src/iocp/config.rs:184-203` - probe-and-cache tests.
- `crates/fast_io/src/iocp/disk_batch.rs:632-...` - end-to-end batch
  exercises (already imports `tempfile` and `CreateFileW`).
- `crates/transfer/src/disk_commit/tests.rs:31-50` -
  `config_iocp_policy_disabled` and `spawn_with_iocp_disabled`,
  asserting the policy plumbing.

## 8. Migration sequence

The closure of #1868 unblocks downstream tasks in the order below. Each
step assumes the prior steps have merged so the test surface and the
runtime probes agree.

1. **#1717-#1721 (merged)** - `OverlappedOp` pinning, completion port
   wrapper, `IocpConfig`, `IocpReader`, `IocpWriter`. Foundation the
   batch builds on.
2. **#1928 (merged)** - Overlapped TCP socket layer
   (`crates/fast_io/src/iocp/socket.rs`). Not consumed by disk-commit
   but cements `OverlappedOp` invariants in production code.
3. **#1821 (merged)** - The dispatch hook in
   `crates/transfer/src/disk_commit/process.rs::make_writer` and the
   `iocp_batch: Option<&mut IocpDiskBatch>` plumbing on `process_file`
   and `process_whole_file`. This is the wiring point.
4. **#1897 (in-flight)** - Symmetric `IocpDiskBatch` parity polish
   against `io_uring/disk_batch.rs`.
5. **#1898 (in-flight)** - Pump-based dispatch
   (`crates/fast_io/src/iocp/pump.rs`). Generalises beyond the batch's
   single-port-single-thread model for future async paths; disk-commit
   stays on the batch surface.
6. **#1929 (in-flight)** - `writer_from_file` reopen-with-overlapped
   path for non-overlapped handles. Affects `IocpOrStdWriter`, not
   disk-commit (the batch always reopens via
   `disk_batch.rs:363-383`).
7. **#1930 (in-flight)** - `ERROR_INSUFFICIENT_BUFFER` retry growth in
   `pump.rs:343-389`. Disk-commit clamps its drain to
   `in_flight.len().min(64)` (`disk_batch.rs:441`) so this is a pump
   concern only.
8. **#1931 / #1932 (open)** - Regression tests for partial writes and
   disk-full. Required before #1900 enables the matrix entry.
9. **#1871 (open)** - High-concurrency stress harness; nightly job,
   gating for declaring the IOCP path production-ready.
10. **#1900 (open)** - Wire `windows-latest` nextest job in
    `.github/workflows/ci.yml` with `--all-features`.
11. **#1899 (open)** - Documentation, `--version` strings, and the
    `--iocp` / `--no-iocp` user knob. `iocp_status_detail`
    (`crates/fast_io/src/lib.rs:188-211`) already produces the
    machine-friendly status.
12. **#1868** - Closes when items 1-8 are merged and 9-11 are filed
    against the actual implementation. Items 3 and 8 are load-bearing.

## 9. Non-goals

- **No ReFS reflink integration (#1389).** The disk-commit thread copies
  bytes off the wire; reflink applies only to the local-copy executor in
  `crates/engine/src/local_copy/` and is independent of IOCP.
- **No COW or copy-on-write modes.** IOCP writes are byte-streaming
  overlapped `WriteFile`s; no clone, no extent duplication.
- **No async runtime in the daemon.** The pump
  (`crates/fast_io/src/iocp/pump.rs`) provides a worker-thread completion
  drain, not a Tokio reactor. Daemon I/O remains driven by the existing
  thread-per-connection model.
- **No new wire-protocol features.** The doc wires an internal disk-write
  backend; nothing on the rsync wire changes.
- **No CLI flag exposure in this task.** `IocpPolicy::Auto` is the only
  user-visible behaviour for #1868; the `--iocp` / `--no-iocp` flags
  belong to #1899.
- **No socket-side IOCP for daemon transports.** The overlapped socket
  layer (`iocp/socket.rs`, #1928) is separate from disk-commit and is
  not wired into the daemon's network thread by this task.

## 10. Risks

- **`OVERLAPPED` lifetime and pinning bugs.** Each in-flight op pins its
  `OVERLAPPED` and its buffer in a `Pin<Box<OverlappedOp>>`
  (`overlapped.rs:16-25`). Dropping the `OverlappedOp` while the kernel
  still owns the buffer causes use-after-free with no `unsafe` warning.
  Mitigation: the `submit_write_batch` loop holds every in-flight op in
  `Vec<Pin<Box<OverlappedOp>>>` (`disk_batch.rs:421`) and only retires
  them after a matching completion. The `Drop` impl
  (`disk_batch.rs:347-354`) flushes synchronously before returning, so
  no completion ever outlives its op. Risk surfaces only if a future
  refactor short-circuits the drain loop on error; new code MUST keep
  the invariant that every submitted op is reaped before its
  `OverlappedOp` is dropped.
- **Completion-port shutdown ordering.** The pump uses a sentinel key
  (`pump.rs:65-70`, `SHUTDOWN_KEY = usize::MAX`) for orderly shutdown
  via `PostQueuedCompletionStatus`. The disk-commit batch sits on a
  smaller path that does not use the pump - it owns its port directly
  and drops it via `CompletionPort::Drop`
  (`completion_port.rs:67-76`). Closing the port while overlapped ops
  are still pending raises `ERROR_ABANDONED_WAIT_0` on the next dequeue;
  `flush_current` always drains before `commit_file` runs
  (`disk_batch.rs:294-323`), and `Drop` calls `flush_current`
  (`disk_batch.rs:351`).
- **`ERROR_INSUFFICIENT_BUFFER` under memory pressure.**
  `GetQueuedCompletionStatusEx` returns this when the entry array is
  smaller than the available completions
  (`error.rs:14-19`, #1930). The disk-commit batch sizes its drain at
  exactly `min(in_flight.len(), 64)` (`disk_batch.rs:441`), so it never
  asks for fewer entries than it has in flight. The pump grows its
  buffer dynamically (`pump.rs:343-389`); the disk-commit batch does
  not need that path. Risk: a future change that decouples the drain
  size from the in-flight count must reintroduce the dynamic growth or
  hit the same bug. The clamp is documented at
  `disk_batch.rs:64-68`.
- **Reopen failures masking real I/O errors.** `ReOpenFile`
  (`disk_batch.rs:363-383`) can fail with `ERROR_SHARING_VIOLATION` if
  the caller's `File` was opened without `FILE_SHARE_*` flags.
  `process.rs::open_output_file`
  (`process.rs:225-249`) routes through `OpenOptions` (default share
  flags) for in-place writes and through `open_tmpfile` (which sets
  share flags via `tempfile`) for temp+rename, so the case should not
  arise; if a future change opens a file exclusively, `make_writer`
  will surface `IocpError::InvalidOperation` (`error.rs:36-47`) rather
  than hang.
- **Synchronous fallback masking degradations.** With `IocpPolicy::Auto`
  (the default) a probe failure silently downgrades to buffered writes.
  Users see no error; performance regresses to the pre-#1868 baseline.
  The mitigation is observability: `iocp_availability_reason()`
  (`config.rs:113-124`) is logged at debug-I/O level 1 by
  `log_iocp_status` (`thread.rs:138-156`) and `iocp_status_detail()`
  appears in `--version` output (`lib.rs:188-199`). Operators who need
  a hard guarantee set `IocpPolicy::Enabled`.
- **Append-mode regressions.** The dispatch in `make_writer` falls back
  to buffered writes when `append_offset > 0`
  (`process.rs:288`). Both batches issue writes at absolute offsets
  starting at zero; allowing them to run in append mode would zero-pad
  the prefix of an existing file. The buffered writer honours the
  `seek` set by `open_output_file` (`process.rs:240-243`). Any future
  change that pre-seeks the batched writers must update both branches
  symmetrically and add a regression test.

## 11. References

file:LINE evidence is inlined throughout sections 1-10. Entry points:

- Disk-commit pipeline: `crates/transfer/src/disk_commit/{mod,config,thread,process,writer,tests}.rs`.
- Reorder buffer: `crates/transfer/src/reorder_buffer.rs`.
- IOCP backend: `crates/fast_io/src/iocp/{mod,config,completion_port,disk_batch,overlapped,pump,error,file_writer,file_factory,file_reader}.rs`,
  with the cross-platform stub at `crates/fast_io/src/iocp_stub.rs`.
- io_uring parity reference: `crates/fast_io/src/io_uring/disk_batch.rs`.
- Public surface and policy enums: `crates/fast_io/src/lib.rs:118-175,188-211,373-453`.
- Feature flags: `Cargo.toml:32-33`, `crates/fast_io/Cargo.toml:39,55`,
  `crates/transfer/Cargo.toml:84-90`.
- Issue refs: #1717-#1721, #1740, #1821, #1868, #1871, #1897, #1898,
  #1899, #1900, #1928, #1929, #1930, #1931, #1932.
