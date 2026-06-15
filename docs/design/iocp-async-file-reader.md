# IOCP async file reader for the sender pipeline (IOCP-H.2)

Design for the sender-side file reader that the Windows transfer
pipeline will use in place of `std::fs::File` + `BufReader`. Closes the
row-3 gap from `docs/design/iocp-pipeline-audit.md`. Mirrors the shape
of `IoUringFileReader` (Linux) so the dispatcher in `fast_io::lib.rs`
can pick the right backend at runtime with no caller changes beyond
opting into a new factory function.

The primitive types (`IocpReader`, `IocpReaderFactory`, `IocpOrStdReader`)
already exist at `crates/fast_io/src/iocp/file_reader.rs` and
`file_factory.rs`. This doc specifies the **public dispatch API** layer
that the sender will consume and the runtime contract that the inner
`IocpReader` must satisfy when scaled up from one-shot reads to a
streaming sender workload.

## Goals

- One typed reader handle per open file, owning its completion port and
  read-ahead pipeline (multiple in-flight `ReadFile` calls).
- Public surface mirrors `std::io::Read` + `std::io::Seek` so existing
  sender code (`generator/context.rs`, `transfer_ops/response.rs`) can
  swap in via the `IocpOrStdReader` enum that already exists.
- Buffer pool integration via `fast_io::BufferPool` so per-file
  allocations do not spike under high concurrency.
- Graceful fallback: `ReadFile`-with-`OVERLAPPED` that returns
  `ERROR_HANDLE_EOF` or `ERROR_BROKEN_PIPE` is treated as a clean EOF,
  not an error.
- Always-available std-I/O fallback when `is_iocp_available()` returns
  false or when the file is below `IOCP_MIN_FILE_SIZE` (64 KB).

## Non-goals

- Async sender-loop redesign. The reader still exposes a synchronous
  `Read` surface to the caller; the IOCP pipeline runs behind it.
  Full async migration is tracked under ASY-* and is independent.
- Socket-side IOCP wiring (covered by IOCP-H.6 and NET-RIO).
- Receiver writer. Symmetric design is the IOCP-H.4 task.

## Public surface

All paths absolute from `crates/fast_io/src/`. New items in **bold**;
existing items unchanged.

```rust
// crates/fast_io/src/lib.rs (already present, behind cfg)
pub use iocp::{IocpReader, IocpReaderFactory, IocpOrStdReader};
pub use iocp::{reader_from_path as iocp_reader_from_path};

// crates/fast_io/src/iocp/file_factory.rs (existing)
pub fn reader_from_path<P: AsRef<Path>>(
    path: P,
    config: &IocpConfig,
) -> io::Result<IocpOrStdReader>;

// New thin wrapper that the sender pipeline calls. Mirrors the
// io_uring `reader_from_path_with_depth` signature so the dispatcher
// at the crate root can route uniformly.
**pub fn iocp_reader_from_path_with_depth<P: AsRef<Path>>(
    path: P,
    config: &IocpConfig,
    depth: Option<u32>,
) -> io::Result<IocpOrStdReader>;**
```

`IocpOrStdReader` already implements the workspace `FileReader` trait
(`crates/fast_io/src/traits.rs`) which in turn requires `Read + Seek`.
No new trait is needed; the dispatch enum carries the IOCP path on
Windows and the std-I/O path everywhere else.

### Constructor contract

```rust
impl IocpReader {
    /// Opens `path` with FILE_FLAG_OVERLAPPED, creates a per-reader
    /// completion port, and primes `depth` read-ahead operations.
    /// `depth` clamps to `[MIN_CONCURRENT_OPS, MAX_CONCURRENT_OPS]` from
    /// `iocp::config`.
    pub fn open<P: AsRef<Path>>(path: P, config: &IocpConfig) -> io::Result<Self>;
}
```

The existing `IocpReader::open` (`file_reader.rs:38`) already does
the open + port-associate handshake. This design adds the read-ahead
priming step so the very first `Read::read` returns immediately rather
than blocking on a single `ReadFile`.

### Read + Seek behaviour

`Read` semantics:

- `read(buf)` first drains the completion port for any finished
  read-ahead operation whose buffer overlaps the current `position`,
  copies bytes into `buf`, advances `position`, and resubmits the
  consumed slot.
- If no completion has arrived, it issues a synchronous
  `GetQueuedCompletionStatus` with a sender-tunable timeout (default
  100 ms, configurable through `IocpConfig::read_timeout`).
- Short reads at file end return whatever bytes were available, then
  a subsequent `read` returning `Ok(0)` signals EOF (std contract).

`Seek` semantics:

- `seek(Start(n))` cancels all in-flight read-aheads whose ranges no
  longer overlap, updates `position`, and re-primes the read-ahead
  pipeline at the new offset.
- `seek(Current(n))` and `seek(End(n))` reduce to `Start` after
  resolving against the cached `size` field.
- Cancellation uses `CancelIoEx` against the file handle; queued
  completions are drained until the cancel-confirmation packet arrives
  to avoid leaking buffers back into the pool.

## Read-ahead pipeline

The IOCP completion port carries `depth` simultaneous reads. Each slot
owns:

- A boxed `OVERLAPPED` carrying the file offset.
- A `PooledBuffer` checked out from `fast_io::BufferPool` at slot
  initialization, returned on read consumption.

Slot rotation uses a ring of `depth` entries; once `read()` consumes the
buffer at the front, the slot is resubmitted at `position + depth *
buffer_size`. The dispatcher in `IocpReader::read` selects the
front-most slot that holds the byte at `position` and copies bytes into
`buf` slice-by-slice across overlapping slots when `buf` straddles a
buffer boundary.

### Buffer pool integration

```rust
let buffer = fast_io::BufferPool::global().checkout(config.buffer_size)?;
```

- `IocpConfig::buffer_size` defaults to 256 KB to match the sender's
  read budget per call.
- Slots return their buffers in their `Drop` impl so a cancelled or
  aborted reader does not leak.
- The pool's RAII `PooledBuffer` guarantees zero-copy reuse across
  successive files.

## Completion handling

Each `IocpReader` owns a single `CompletionPort`
(`crates/fast_io/src/iocp/completion_port.rs:23`). The completion
handler is inlined into the `read` path (no separate pump thread) to
keep the sender pipeline synchronous from the caller's perspective.

Flow:

1. `read()` dequeues entries via `GetQueuedCompletionStatus`.
2. Each entry's `lpOverlapped` is cast back to the slot's boxed
   `OVERLAPPED`, the byte count is read from `dwNumberOfBytesTransferred`,
   and the slot is marked Ready.
3. The slot whose offset matches `position` is consumed; its buffer is
   memcpy'd into the caller's `buf` and the slot is resubmitted for the
   next offset.
4. If `GetQueuedCompletionStatus` returns `FALSE`, the error is
   classified via `classify_overlapped_error`
   (`crates/fast_io/src/iocp/error.rs`).

## EOF and pipe-closed fallback

Two completion-port outcomes count as clean EOF rather than errors:

- `ERROR_HANDLE_EOF` (`38`) - `ReadFile` reached the file's end. Treat
  the current and all subsequent `read()` calls as `Ok(0)`.
- `ERROR_BROKEN_PIPE` (`109`) - target handle was a pipe (only reachable
  if a future caller hands an `IocpReader` a pipe). Same handling.

MSDN reference for these specific errors:

- ReadFile (synchronous and overlapped contract):
  https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile
- GetQueuedCompletionStatus:
  https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getqueuedcompletionstatus
- CancelIoEx:
  https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex
- System Error Codes (1300-1699), includes ERROR_HANDLE_EOF:
  https://learn.microsoft.com/en-us/windows/win32/debug/system-error-codes--0-499-
- CreateIoCompletionPort:
  https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-createiocompletionport

`classify_overlapped_error` already encodes the EOF mapping; the new
`read()` path just needs to translate the classified result into the
`Read` trait's `Ok(0)` shape and stop dispatching new read-aheads.

Other failures (access denied, invalid handle, etc.) propagate as
`io::Error` with the underlying Win32 error preserved via
`io::Error::from_raw_os_error`.

## Fallback to std

`IocpOrStdReader::Std(StdFileReader)` is selected when:

- `is_iocp_available()` returns false. The check is cached so the cost
  is one atomic load per open.
- The file is smaller than `IOCP_MIN_FILE_SIZE` (64 KB). Below that
  threshold the completion-port setup overhead exceeds the async win,
  measured in `iocp_vs_stdio` bench.
- The `iocp` Cargo feature is disabled. In that build the stub
  module's `IocpReaderFactory` always returns `Std`.

These dispatch arms are already wired in `file_factory.rs`; the new
`iocp_reader_from_path_with_depth` function inherits them.

## Integration points

The sender pipeline will swap call sites from
`fast_io::reader_from_path_with_depth` (currently routed through the
io_uring path on Linux, std-I/O on Windows) to a new dispatcher that
picks the IOCP path on Windows:

- `crates/transfer/src/generator/context.rs:464` (signature scan).
- `crates/transfer/src/generator/context.rs:509` (literal-data
  read-back).
- `crates/transfer/src/map_file/mmap.rs` fallback path when
  `MmapReader::open` returns `ErrorKind::Unsupported` for files that
  cannot be memory-mapped (rare, but real on Windows for files on
  network shares).

The dispatcher selection happens behind a Cargo feature
`iocp-data-reads` (parallel to `iouring-data-reads`) so the bake-window
rollout strategy mirrors IUD-5.

## Testing

Unit tests live alongside `file_reader.rs`:

- 64 KB read across one buffer boundary, validates correct stitching.
- Seek forward past one buffer, then `read()`, asserts re-prime fires.
- Seek backward into a cancelled buffer, asserts pool buffer is
  returned and re-fetched.
- File size below `IOCP_MIN_FILE_SIZE` returns the std path.
- Force `ERROR_HANDLE_EOF` via a sized small file and assert
  `read(&mut [0; 4096]) -> Ok(0)`.

Integration tests run under the Windows CI cell `WPG-6` once the sender
swap-in lands behind `iocp-data-reads`.

## Risk and rollback

- Rollback: disable the feature flag. The dispatcher reverts to
  `StdFileReader`. No persisted state, no wire-protocol effect.
- Cross-platform: non-Windows builds compile against the stub which
  already routes to `StdFileReader`, so no caller breakage.
- Memory: per-reader allocation is `depth * buffer_size` from the
  pool. At the default `depth=4` and `buffer_size=256 KB` that is 1 MB
  per open file. The bench bake should validate peak RSS at typical
  sender concurrency (16-32 concurrent files) stays under 32 MB.

## Cross-references

- `docs/design/iocp-pipeline-audit.md` (IOCP-H.1) - inventory and gap
  table.
- `docs/design/iocp-transfer-pipeline-wiring.md` (#1868) - companion
  design for the receiver disk-commit path.
- `crates/fast_io/src/iocp/file_reader.rs` - existing `IocpReader`
  primitive.
- `crates/fast_io/src/iocp/file_factory.rs` - existing
  `IocpReaderFactory` + `reader_from_path`.
- `crates/fast_io/src/iocp/error.rs` - `classify_overlapped_error`
  (handles `ERROR_HANDLE_EOF`).
- `crates/fast_io/src/io_uring/file_factory.rs` - reference
  implementation for the symmetric `reader_from_path_with_depth`
  signature on Linux.
