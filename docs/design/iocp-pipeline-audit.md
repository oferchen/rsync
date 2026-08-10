# IOCP transfer pipeline audit (IOCP-H.1)

Inventory of every transfer surface that touches a file handle or socket
on Windows, classified by whether IOCP is wired in production or not.
Compares the wiring state on master against the "fully async via IOCP"
target. Companion to `iocp-async-file-reader.md` (IOCP-H.2). Cross-
references the existing `iocp-transfer-pipeline-wiring.md` (#1868) which
covers the receiver `disk_commit` batched-write path.

## Scope

- Real Windows impl: `crates/fast_io/src/iocp/` (`iocp` feature, Windows).
- Cross-platform stub: `crates/fast_io/src/iocp_stub/` (returns `Ok(None)`).
- All transfer paths route file I/O through `fast_io` factories or
  through `MapFile` / `MmapReader`. Sockets route through `rsync_io` or
  `russh` and do NOT pass through `fast_io::iocp::socket`.

## Surface table

| # | Surface | Current impl | File path + lines | Gap vs fully-async IOCP |
|---|---------|--------------|-------------------|-------------------------|
| 1 | Receiver disk-commit writer (per-file) | Sync `fast_io::writer_from_file_with_depth` -> on Windows this dispatches into `io_uring_stub::file_factory::writer_from_file_with_depth` which returns `StdFileWriter` (`std::fs::File` + `BufWriter`). IOCP not invoked. | `crates/transfer/src/transfer_ops/response.rs:125`; stub at `crates/fast_io/src/io_uring_stub/file_factory.rs:236` | Need an IOCP `IocpWriterFactory`-backed path. `IocpWriter` exists at `crates/fast_io/src/iocp/file_writer.rs:30` with `FILE_FLAG_OVERLAPPED` + per-writer completion port, but no caller routes through it. Closing this gap is the IOCP-H.4 task. |
| 2 | Receiver disk-commit batched writes | `fast_io::IoUringDiskBatch` on Linux; `IocpDiskBatch` is implemented but not wired through `make_writer` in `disk_commit/process.rs`. | `crates/fast_io/src/iocp/disk_batch/` (impl exists); `crates/transfer/src/disk_commit/process.rs:269` (`make_writer`) | Documented separately in `docs/design/iocp-transfer-pipeline-wiring.md` (#1868). The `Writer::Iocp` variant landed on the receiver side but full default-on rollout is open. |
| 3 | Sender file-read (signature + literal-data scan) | Sync `std::fs::File` + `BufReader` via `fast_io::reader_from_path_with_depth` -> Windows path returns `StdFileReader`. Mmap fallback via `MmapReader` for basis files. No IOCP read. | `crates/transfer/src/generator/context.rs:464, 509`; `crates/transfer/src/map_file/mmap.rs:38`; stub at `crates/fast_io/src/io_uring_stub/file_factory.rs:265` | `IocpReader` exists at `crates/fast_io/src/iocp/file_reader.rs:29` with `FILE_FLAG_OVERLAPPED`, per-reader completion port, and read-ahead support, but no caller routes through it. **This is the IOCP-H.2 design target.** |
| 4 | Sender basis-mmap reader (delta lookup) | `fast_io::MmapReader` over `std::fs::File` -> `MapView` mapping. No IOCP. | `crates/transfer/src/map_file/mmap.rs:22-39` | Mmap on Windows uses page-fault-driven I/O; IOCP cannot accelerate the fault itself. Eligible only when basis exceeds `MmapReader` size cap and we fall back to `BufferedMap`/streaming reads, in which case IOCP-H.2 covers it. |
| 5 | Daemon file-serve (sender to socket) | Sync `std::fs::File::read` -> userspace -> `TcpStream::write_all`. `iocp::transmit_file` and `iocp::IocpSocketWriter` exist but no transfer/daemon caller invokes them. | `crates/fast_io/src/iocp/transmit_file.rs:98`; `crates/fast_io/src/iocp/socket.rs:231` | The memory note recorded WIN-S.LAND.1.c as wired, but a workspace-wide grep for `transmit_file` / `try_transmit_file` returns zero consumers under `crates/daemon/`, `crates/transfer/`, `crates/engine/`. This is a documentation drift; the actual wire-in is the IOCP-H.6 task. |
| 6 | Daemon TCP accept (listener) | Sync `TcpListener::accept` blocking each acceptor thread. `iocp::CompletionPump` + `IocpSocketReader::associate` exist. | `crates/daemon/src/` (accept loop); `crates/fast_io/src/iocp/pump.rs` (pump infra) | Acceptor never associates the listener handle with an IOCP port. Async accept would close the D10K-3/4 thread-per-connection ceiling but is out of scope for IOCP-H (covered by DASYNC.* + NET-RIO). |
| 7 | Daemon multiplex socket I/O (per connection) | Sync `Read`/`Write` over `TcpStream` wrapped by `rsync_io::MultiplexedReader` / `MultiplexedWriter`. No IOCP socket reader/writer in the path. | `crates/rsync_io/src/` (multiplex frames); `crates/fast_io/src/iocp/socket.rs` (unused IOCP impl) | The IOCP `WSARecv`/`WSASend` path is fully built but unused. Wiring requires touching every multiplex reader/writer and is a separate large-scope task (NET-RIO covers Registered I/O, which subsumes the IOCP socket path). Not in IOCP-H scope. |
| 8 | SSH stdio multiplex (russh bridge) | `tokio::spawn_blocking` shim around russh. Windows-specific IOCP paths do not apply because russh owns the socket. | `crates/transport/src/ssh/` | Russh-async migration tracked separately (RUSSH-ASY.*). Not an IOCP target. |
| 9 | Local-copy executor file copy | `fast_io::copy_file_optimized` -> `CopyFileExW` via `windows-rs`. Synchronous from caller's perspective but kernel-internal pipelining is efficient. | `crates/engine/src/local_copy/` (dispatcher); `crates/fast_io/src/windows/copy_file.rs` (impl) | `CopyFileExW` already uses the kernel's optimized copy. No IOCP-async benefit large enough to justify rewiring. Out of scope. |
| 10 | Metadata-only `IocpDiskBatch` (statx-style ops) | Wired on Windows via `make_writer` Iocp branch. | `crates/fast_io/src/iocp/disk_batch/` | Already wired. No gap. |

## Gap summary

Three real gaps remain after subtracting items already covered by other
tasks and items where IOCP brings no measurable benefit:

1. **Sender file-read (row 3)** - IOCP-H.2 designs the API; IOCP-H.3
   implements it behind a feature flag.
2. **Receiver per-file writer (row 1)** - IOCP-H.4 designs; IOCP-H.5
   implements. Symmetric to row 3 in API shape.
3. **Daemon `TransmitFile` (row 5)** - IOCP-H.6 wires it as the default
   sender-to-socket path on Windows. The primitive is built and tested
   (`crates/fast_io/src/iocp/transmit_file.rs`); only the call site is
   missing.

Rows 2 and 10 are already wired or documented in
`iocp-transfer-pipeline-wiring.md`. Rows 6, 7, 8, 9 are explicitly out
of scope for IOCP-H and tracked by other initiatives.

## Probes and gates

Every IOCP path must funnel through `fast_io::is_iocp_available()`
(`crates/fast_io/src/iocp/config.rs:45`), which caches the runtime
detection result. Callers must keep the std-I/O fallback even when the
`iocp` feature is compiled in; the IOCP wrapper types
(`IocpOrStdReader`, `IocpOrStdWriter`) already encode this dispatch.

When `iocp` is disabled at build time, callers compile against
`iocp_stub::` which mirrors the public surface but never opens a
completion port, ensuring no Windows-only intrinsic leaks into a
default cross-platform build.

## Out-of-band findings

- `crates/fast_io/src/lib.rs:412` re-exports `iocp::file_factory::reader_from_path` and `writer_from_file` as `iocp_reader_from_path` / `iocp_writer_from_file`. These are the public IOCP entry points but have no consumers outside the crate's own tests/benches.
- `crates/fast_io/benches/iocp_vs_stdio.rs` exercises the factory directly, so the perf delta of routing through it has been measured in isolation but never end-to-end.
- The memory-note claim of "WIN-S.LAND.1.c: Wire iocp::transmit_file into Windows daemon transmit path (P1) - completed" is contradicted by a workspace grep. Either the wire-up regressed or the note records an aspirational state. IOCP-H.6 must verify before re-claiming completion.

## Cross-references

- `docs/design/iocp-transfer-pipeline-wiring.md` (#1868) covers the receiver `disk_commit` `IocpDiskBatch` rollout in detail and is the authoritative spec for row 2.
- `docs/design/iocp-async-file-reader.md` (IOCP-H.2) specifies the public API for row 3.
- `docs/design/wpg-7c-iocp-gap-list.md` enumerates io_uring opcodes that have no Windows equivalent. Not all are transfer-pipeline gaps; rows above are the subset that gate real transfer throughput.
