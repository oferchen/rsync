# IOCP disk-full regression tests (#1932)

This note plans the disk-full regression suite for the Windows IOCP
disk-commit path wired by #1868 and tracked as the open task on row
#1932 of `docs/design/iocp-transfer-pipeline-wiring.md`. The Linux
counterpart already exists as the `ENOSPC` coverage filed under #1148;
this work adds an analogous suite on top of the IOCP backend so the
`windows-latest` matrix entry (#1900) can ship with confidence.

## 1. Failure mode under test

`IocpDiskBatch::submit_write_batch`
(`crates/fast_io/src/iocp/disk_batch.rs:406`) issues overlapped
`WriteFile` calls. When the destination volume is full, the kernel
fails the completion with Win32 `ERROR_DISK_FULL` (0x70). The drain
loop maps the per-op status to `io::Error::from_raw_os_error(112)`,
which surfaces through `IocpDiskBatch::write_data` /
`commit_file` and bubbles up `Writer::Iocp::write_chunk` /
`finish` (`crates/transfer/src/disk_commit/writer.rs:177-244`) into
`process_file` (`crates/transfer/src/disk_commit/process.rs:38-68`).
The disk thread then propagates the error to the receiver side.

Required behaviour, mirroring the Linux `ENOSPC` path validated for
#1148:

- Error is categorised as fatal, not recoverable. Existing free
  function `categorize_io_error` in `crates/transfer/src/error.rs`
  routes `ErrorKind::StorageFull` into `DeltaFatalError` (see
  `crates/transfer/src/error.rs::categorize_disk_full_as_fatal`,
  `crates/transfer/src/receiver/tests.rs::error_categorization_disk_full_is_fatal`).
- No panic, no `unwrap`, no wedged completion port. The disk thread
  drains every in-flight op before returning, honouring the
  `OverlappedOp` lifetime invariant noted in section 10 of
  `docs/design/iocp-transfer-pipeline-wiring.md`.
- Exit code maps to upstream rsync `ERROR_FILEIO` (11) via
  `crates/transfer/src/error.rs` -> `core` -> the CLI exit-code
  table, matching upstream `rsync.h::RERR_FILEIO`.

## 2. Test surface

Integration test file:
`crates/transfer/tests/iocp_disk_full.rs`. Gate the entire module with
`#![cfg(all(target_os = "windows", feature = "iocp"))]` so the file is
inert on Linux and macOS, and on Windows builds without the `iocp`
default feature.

Helpers, all already public for the existing IOCP tests:

- `transfer::disk_commit::config::DiskCommitConfig` with
  `iocp_policy = IocpPolicy::Enabled` and `io_uring_policy =
  IoUringPolicy::Disabled`.
- `transfer::disk_commit::thread::spawn_disk_thread` for the
  end-to-end channel wiring.
- `fast_io::iocp::config::is_iocp_available` to skip the test
  gracefully on hosts where the probe returns `false` (mirrors the
  Linux `io_uring_available` skip pattern in #1148).

## 3. Limited-capacity backing store

Windows lacks `tmpfs`; the test creates a fixed-size virtual disk via
the built-in VHD APIs. Two equivalent paths:

- `Win32_VHD` through `windows::Win32::Storage::Vhd::CreateVirtualDisk`
  (already an indirect dependency of `fast_io`). Mount the VHD with
  `AttachVirtualDisk`, format NTFS via `FormatEx` from
  `fmifs.dll`, and assign a drive letter through `SetVolumeMountPoint`.
  Capacity: 4 MiB - large enough to seat the 256 KiB IOCP buffer plus
  metadata, small enough to overflow within one transfer.
- Fallback when the test runner cannot mount VHDs (CI limitation):
  use `SetFileValidData` plus `FSCTL_SET_COMPRESSION` on a sparse file
  in `%TEMP%` as the destination, then write enough bytes to exceed
  the configured quota set via `DiskQuotaControl` on `%TEMP%`'s
  volume. The test prefers VHD; quota fallback is gated behind a
  capability probe.

Cleanup uses `tempfile::TempDir` for any host-side scratch and
`DetachVirtualDisk` + `DeleteFile` for the VHD itself in a
`scopeguard`-style RAII handle, so a panic mid-test never leaks the
mount.

## 4. Test cases

1. `iocp_disk_full_surfaces_fatal_error`: copy a 16 MiB stream into the
   4 MiB volume. Assert the disk thread returns
   `DeltaTransferError::Fatal(DeltaFatalError::Io { .. })` whose source
   matches `ErrorKind::StorageFull` and `raw_os_error() ==
   Some(ERROR_DISK_FULL as i32)`.
2. `iocp_disk_full_drains_completion_port`: after the failure, assert
   `IocpDiskBatch::Drop` returns without blocking and that no ops
   remain by inspecting `bytes_written_with_pending` ==
   `bytes_written` (`crates/fast_io/src/iocp/disk_batch.rs:286`).
3. `iocp_disk_full_does_not_panic`: wrap the disk thread join in
   `std::panic::catch_unwind` and assert no panic payload.
4. `iocp_disk_full_exit_code_maps_to_file_io`: drive the failure
   through `core::session()` and assert the resulting exit code is
   `ExitCode::FileIo` (11), matching upstream `RERR_FILEIO`.
5. `iocp_disk_full_partial_file_cleaned_up`: assert the receiver's
   temp file under `--partial-dir` (or the in-place destination, when
   `--inplace` is set) is removed or truncated per the existing
   receiver semantics in `crates/transfer/src/receiver/cleanup.rs`.

## 5. Cross-references

- Linux `ENOSPC` parity test: #1148. The IOCP suite reuses the same
  assertion shape - fatal categorisation, no panic, exit code 11 - so
  Linux and Windows stay symmetric.
- IOCP wiring design and task table: `#1932` row in section 2.3 of
  `docs/design/iocp-transfer-pipeline-wiring.md`.
- Partial-write companion suite: #1931, lives next door in
  `crates/fast_io/src/iocp/disk_batch.rs::tests`.
- CI matrix entry that consumes this suite: #1900.
