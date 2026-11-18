# Windows I/O path map - IOCP versus standard I/O

Measured 2026-11-18 against `master` at `0335c2219`. Each row names the
I/O path, what it uses on Windows, and every gate that selects it, with
the source line that decides. Read from source and cross-checked with the
commands in section 4; not measured on a Windows machine.

IOCP is compiled only for `target_os = "windows"` with the `iocp` cargo
feature (`crates/fast_io/src/lib.rs:339-343`). The feature is in the
binary's default set (`Cargo.toml:36`) and forwards to the crates that
use it (`Cargo.toml:101`, `crates/transfer/Cargo.toml:114`). Without it
`fast_io::iocp` is the stub in `crates/fast_io/src/iocp_stub/`, whose
`is_iocp_available()` returns false, so every path below takes its
standard-I/O arm.

## 1. Paths that use IOCP

| Path | IOCP primitive | Gates, all required | Otherwise |
|------|----------------|---------------------|-----------|
| Network receiver file writes (daemon, SSH and client receivers share the disk-commit thread) | `fast_io::IocpDiskBatch`, overlapped `WriteFile` on one completion port per disk thread | Build: `windows` + `iocp` (`crates/transfer/src/disk_commit/writer.rs:157-160`). Policy: `DiskCommitConfig::iocp_policy` is not `Disabled` (`crates/transfer/src/disk_commit/thread.rs:116-123`); it defaults to `Auto` (`crates/transfer/src/disk_commit/config.rs:292`) and nothing outside tests sets it. Runtime: `IocpDiskBatch::try_new` succeeds, which needs `is_iocp_available()` (`crates/fast_io/src/iocp/disk_batch/mod.rs:174-179`). Per file: not sparse, no append offset, not `--inplace` (`crates/transfer/src/disk_commit/process/file_ops.rs:1153-1162`) | `Writer::Buffered`, a 256 KB buffered `std::fs::File` writer (`crates/transfer/src/disk_commit/writer.rs:152`) |
| Network sender file reads | `fast_io::IocpReader` through `iocp_reader_from_path` | Build: `windows` (`crates/transfer/src/generator/context.rs:1375`). Per file: `--open-noatime` off, size at least 1 MiB, and the io_uring policy not `Disabled` - so `--no-io-uring` also turns this reader off (`crates/transfer/src/generator/context.rs:1379-1383`). Runtime, inside the factory under `Auto`: `is_iocp_available()`, size at least `IOCP_MIN_FILE_SIZE` (64 KiB), and the overlapped open succeeds (`crates/fast_io/src/iocp/file_factory.rs:431-438`) | A plain `std::fs::File` opened by `source_open()` (`crates/transfer/src/generator/context.rs:1391`) |

The receiver logs its choice at `--debug=io` level 1
(`crates/transfer/src/disk_commit/thread.rs:180-198`). The sender's reader
choice is not logged.

## 2. Paths that never use IOCP

| Path | What it uses on Windows | Decided at |
|------|-------------------------|------------|
| Local copy, whole-file data | ReFS block clone when the destination volume is ReFS, then `CopyFileExW` (with `COPY_FILE_NO_BUFFERING` above 4 MiB), then `std::fs::copy` | `crates/fast_io/src/platform_copy/dispatch.rs:106-128`, reached from `crates/engine/src/local_copy/clonefile.rs:70` and `crates/engine/src/local_copy/win_copy.rs:123` |
| Sockets: daemon TCP, `rsync://` client, SSH child pipes | The standard library's sockets and pipes, or the async runtime's where one drives the connection; no `fast_io::iocp` type. The `fast_io` socket factories are Unix-only (`crates/fast_io/src/lib.rs:617-621`) | No Windows arm exists |
| Sparse, append and `--inplace` receiver writes | `Writer::Buffered` (they need `Seek`) | `crates/transfer/src/disk_commit/process/file_ops.rs:1153-1162` |
| Sender reads below 1 MiB, or with `--open-noatime` or `--no-io-uring` | `std::fs::File` | `crates/transfer/src/generator/context.rs:1380-1383` |

## 3. IOCP code with no production caller

These are built and exported but nothing outside `crates/fast_io/src/iocp/`
calls them, so they do not affect a transfer:

- `iocp::socket` (overlapped `WSARecv`/`WSASend`) and `iocp::rio`
  (Registered I/O): `crates/fast_io/src/iocp/mod.rs:40-42`.
- `try_transmit_file` (`TransmitFile`), behind the non-default
  `transmitfile` feature: `crates/fast_io/src/lib.rs:611-612`.
- `IocpWriterFactory`, `IocpReaderFactory`, `iocp_writer_from_file`:
  `crates/fast_io/src/lib.rs:605-615`. Only `iocp_reader_from_path` is
  called (section 1).

Statements elsewhere that Windows socket I/O goes through IOCP describe
this unwired code, not a shipped path.

## 4. How to re-check

Run from the repository root. The first lists every IOCP consumer outside
`fast_io`; it must print only `crates/transfer/src/disk_commit/` and
`crates/transfer/src/generator/context.rs`. The second must print
nothing.

```sh
grep -rnE 'Iocp(Reader|Writer)Factory|IocpOrStd(Reader|Writer)|IocpDiskBatch|iocp_reader_from_path|iocp_writer_from_file|IocpPolicy' \
  crates --include='*.rs' | grep -vE 'crates/fast_io/|/tests/|tests[a-z_]*\.rs|benches/' | cut -d: -f1 | sort -u

grep -rnE 'iocp::(socket|rio|transmit_file)|try_transmit_file|IocpSocket|RioSocket' \
  crates --include='*.rs' | grep -vE 'crates/fast_io/src/(iocp|iocp_stub)/|crates/fast_io/src/lib\.rs|/tests/|benches/'
```
