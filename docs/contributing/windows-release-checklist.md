# Windows manual release validation checklist

Manual test checklist for validating oc-rsync Windows releases. Run on
a physical or VM Windows host with NTFS. CI covers automated checks
(see `.github/workflows/ci.yml` Windows cells); this document covers
manual validation that cannot run in CI.

Prerequisites:

- Windows 10/11 or Windows Server 2019+ with NTFS.
- `oc-rsync.exe` release binary built with `--features iocp`.
- Upstream `rsync` available (MSYS2 `pacman -S rsync` or standalone).
- PowerShell 7+ for running test commands.
- Developer Mode enabled (required for symlink creation without elevation).
- A test directory with at least 1 GiB free space.

---

## 1. Basic functionality

### 1.1 Single file copy

| Field | Value |
|-------|-------|
| **Description** | Copy a single file to a new destination |
| **Command** | `oc-rsync.exe -av testfile.bin dest\` |
| **Expected outcome** | File copied with preserved timestamps; exit code 0 |
| **Pass/fail criteria** | `fc /b testfile.bin dest\testfile.bin` returns no differences; mtime matches source |

### 1.2 Recursive directory sync

| Field | Value |
|-------|-------|
| **Description** | Sync a directory tree with nested subdirectories |
| **Command** | `oc-rsync.exe -av src_tree\ dest_tree\` |
| **Expected outcome** | All files and directories replicated; permissions and timestamps preserved |
| **Pass/fail criteria** | `robocopy src_tree dest_tree /MIR /L` reports zero differences; exit code 0 |

### 1.3 Delete mode

| Field | Value |
|-------|-------|
| **Description** | Remove destination files not present in source |
| **Command** | Create `dest\extra.txt`, then run `oc-rsync.exe -av --delete src\ dest\` |
| **Expected outcome** | `extra.txt` deleted from destination |
| **Pass/fail criteria** | `dest\extra.txt` does not exist after sync; exit code 0 |

### 1.4 Incremental update (quick-check)

| Field | Value |
|-------|-------|
| **Description** | Re-run sync with no source changes; verify no-op |
| **Command** | `oc-rsync.exe -av src\ dest\` (second run, no source changes) |
| **Expected outcome** | Zero files transferred; quick-check skips all |
| **Pass/fail criteria** | Output shows no file transfer lines; total transferred size is 0 |

### 1.5 Checksum mode

| Field | Value |
|-------|-------|
| **Description** | Force checksum comparison instead of quick-check |
| **Command** | `oc-rsync.exe -avc src\ dest\` |
| **Expected outcome** | All files verified by checksum; no transfers if content matches |
| **Pass/fail criteria** | Exit code 0; verbose output shows checksum verification |

---

## 2. IOCP path validation

### 2.1 Verify IOCP is active

| Field | Value |
|-------|-------|
| **Description** | Confirm the IOCP I/O dispatch path is used instead of synchronous fallback |
| **Command** | `oc-rsync.exe -av -vv large_file_100mb.bin dest\ 2>&1 \| findstr "IOCP"` |
| **Expected outcome** | Debug output mentions IOCP path activation |
| **Pass/fail criteria** | Log contains "IOCP" or "completion port" references; no "falling back to synchronous" messages |

### 2.2 IOCP with large file (above 64 KB threshold)

| Field | Value |
|-------|-------|
| **Description** | Transfer a file larger than `IOCP_MIN_FILE_SIZE` (64 KB) to confirm async I/O |
| **Command** | `oc-rsync.exe -av bigfile_1gb.bin dest\` |
| **Expected outcome** | Transfer completes using IOCP path; throughput consistent with disk bandwidth |
| **Pass/fail criteria** | No errors; throughput within expected range for the storage device |

### 2.3 IOCP with many small files (below threshold)

| Field | Value |
|-------|-------|
| **Description** | Transfer 10,000 files under 64 KB each to confirm graceful sync I/O fallback |
| **Command** | `oc-rsync.exe -av small_files_dir\ dest\` |
| **Expected outcome** | All files transferred via standard buffered I/O path; no IOCP overhead |
| **Pass/fail criteria** | All 10,000 files present in destination; exit code 0; no errors |

### 2.4 IOCP under concurrent load

| Field | Value |
|-------|-------|
| **Description** | Run multiple oc-rsync instances targeting separate destinations |
| **Command** | Start 4 parallel instances: `Start-Process oc-rsync.exe -ArgumentList '-av','src\','dest_N\'` for N=1..4 |
| **Expected outcome** | All instances complete without IOCP resource contention errors |
| **Pass/fail criteria** | All 4 processes exit 0; no "access denied" or "handle invalid" errors |

---

## 3. Long path handling

### 3.1 Path exceeding MAX_PATH (260 characters)

| Field | Value |
|-------|-------|
| **Description** | Transfer a file whose full path exceeds 260 characters |
| **Command** | Create deeply nested source: `mkdir src\a\b\c\...` (total > 260 chars), place a file, then `oc-rsync.exe -av src\ dest\` |
| **Expected outcome** | File transferred successfully; `\\?\` prefix applied internally |
| **Pass/fail criteria** | File exists at full destination path; content matches; exit code 0 |

### 3.2 Long filename component (255 characters)

| Field | Value |
|-------|-------|
| **Description** | Transfer a file with a 255-character filename |
| **Command** | Create file with 255-char name in source, then `oc-rsync.exe -av src\ dest\` |
| **Expected outcome** | File transferred with full filename preserved |
| **Pass/fail criteria** | `Get-ChildItem dest\ -Recurse` shows the 255-char filename; content matches |

### 3.3 Long path with --delete

| Field | Value |
|-------|-------|
| **Description** | Delete a destination file with a path exceeding MAX_PATH |
| **Command** | Remove the long-path file from source, then `oc-rsync.exe -av --delete src\ dest\` |
| **Expected outcome** | Long-path file deleted from destination |
| **Pass/fail criteria** | File no longer exists at destination; exit code 0 |

---

## 4. NTFS-specific tests

### 4.1 Case-insensitive collision detection

| Field | Value |
|-------|-------|
| **Description** | Transfer source containing `readme.txt` and `README.txt` (case collision) |
| **Command** | Create both files on a case-sensitive source (e.g., ext4 via WSL), then `oc-rsync.exe -av src/ dest\` |
| **Expected outcome** | Collision detected; warning emitted; one file wins deterministically |
| **Pass/fail criteria** | Warning message about case collision in output; no crash; exit code 0 or 23 (partial transfer) |

### 4.2 Alternate Data Streams with -X

| Field | Value |
|-------|-------|
| **Description** | Transfer files with NTFS Alternate Data Streams using the xattr flag |
| **Command** | Create ADS: `echo data > src\file.txt:stream1`, then `oc-rsync.exe -avX src\ dest\` |
| **Expected outcome** | ADS preserved on destination |
| **Pass/fail criteria** | `Get-Content dest\file.txt:stream1` returns "data"; `dir /r dest\` shows the stream |

### 4.3 Alternate Data Streams without -X (one-shot warning)

| Field | Value |
|-------|-------|
| **Description** | Transfer files with ADS without -X; verify one-shot warning |
| **Command** | `oc-rsync.exe -av src_with_ads\ dest\` (no -X flag) |
| **Expected outcome** | ADS silently dropped; one-shot warning emitted |
| **Pass/fail criteria** | Warning about ADS being dropped appears once in output; ADS not present on destination |

### 4.4 Reparse points (junctions)

| Field | Value |
|-------|-------|
| **Description** | Sync a directory containing an NTFS junction |
| **Command** | Create junction: `mklink /j src\junction target_dir`, then `oc-rsync.exe -av src\ dest\` |
| **Expected outcome** | Junction followed like a directory symlink; target contents transferred |
| **Pass/fail criteria** | Destination contains the junction target's files; exit code 0 |

### 4.5 Sparse file preservation

| Field | Value |
|-------|-------|
| **Description** | Transfer a sparse file and verify hole preservation |
| **Command** | Create sparse file with `FSCTL_SET_SPARSE` + `FSCTL_SET_ZERO_DATA`, then `oc-rsync.exe -avS src\ dest\` |
| **Expected outcome** | Destination file is sparse; disk usage less than logical size |
| **Pass/fail criteria** | `fsutil sparse queryflag dest\sparse.bin` reports "set"; physical size < logical size |

---

## 5. Permission and ACL tests

### 5.1 POSIX permission bit round-trip

| Field | Value |
|-------|-------|
| **Description** | Verify POSIX mode bits map correctly to NTFS read-only attribute |
| **Command** | Sync a read-only file from Linux source: `oc-rsync.exe -avp src/ dest\` |
| **Expected outcome** | Read-only attribute set on destination for files without write bit |
| **Pass/fail criteria** | `attrib dest\readonly.txt` shows `R` attribute; writable files do not have `R` |

### 5.2 DACL round-trip with -A

| Field | Value |
|-------|-------|
| **Description** | Transfer files with explicit NTFS DACLs and verify preservation |
| **Command** | Set explicit DACL: `icacls src\secret.txt /grant "Users:(R)"`, then `oc-rsync.exe -avA src\ dest\` |
| **Expected outcome** | DACL ACEs replicated on destination file |
| **Pass/fail criteria** | `icacls dest\secret.txt` shows matching explicit ACEs; inherited ACEs may differ |

### 5.3 Owner and group with -o -g

| Field | Value |
|-------|-------|
| **Description** | Transfer files preserving owner/group SID information |
| **Command** | `oc-rsync.exe -avog src\ dest\` |
| **Expected outcome** | Owner SID matches source (requires `SeRestorePrivilege` or admin) |
| **Pass/fail criteria** | `(Get-Acl dest\file.txt).Owner` matches source owner; or graceful permission error if unprivileged |

### 5.4 --chmod modifier

| Field | Value |
|-------|-------|
| **Description** | Apply chmod modifiers and verify NTFS attribute mapping |
| **Command** | `oc-rsync.exe -av --chmod=a-w src\ dest\` |
| **Expected outcome** | All destination files marked read-only |
| **Pass/fail criteria** | All files in `dest\` have the read-only attribute set |

---

## 6. Network tests

### 6.1 Daemon mode - local push

| Field | Value |
|-------|-------|
| **Description** | Start oc-rsync in daemon mode and push files to a module |
| **Command** | Start daemon: `oc-rsync.exe --daemon --config=oc-rsyncd.conf --no-detach`, then push: `oc-rsync.exe -av src\ rsync://localhost/testmod/` |
| **Expected outcome** | Files appear in the module's configured path |
| **Pass/fail criteria** | All source files present in module path; daemon log shows completed transfer; exit code 0 |

### 6.2 Daemon mode - pull

| Field | Value |
|-------|-------|
| **Description** | Pull files from a running daemon module |
| **Command** | `oc-rsync.exe -av rsync://localhost/testmod/ dest\` |
| **Expected outcome** | Files downloaded from module to local destination |
| **Pass/fail criteria** | Destination matches module contents; exit code 0 |

### 6.3 Daemon mode - --max-connections admission

| Field | Value |
|-------|-------|
| **Description** | Verify connection admission gating under load |
| **Command** | Configure `max connections = 2` in `oc-rsyncd.conf`, start daemon, open 3 concurrent clients |
| **Expected outcome** | Third client receives "max connections reached" error |
| **Pass/fail criteria** | Two clients complete successfully; third gets error 5 (connection refused) or appropriate message |

### 6.4 SSH mode via russh - push

| Field | Value |
|-------|-------|
| **Description** | Push files over SSH to a remote host using the built-in russh transport |
| **Command** | `oc-rsync.exe -av -e ssh src\ user@remote:/tmp/dest/` |
| **Expected outcome** | Files transferred over SSH; SSH authentication succeeds |
| **Pass/fail criteria** | Remote `/tmp/dest/` contains all source files; exit code 0 |

### 6.5 SSH mode via russh - pull

| Field | Value |
|-------|-------|
| **Description** | Pull files over SSH from a remote host |
| **Command** | `oc-rsync.exe -av -e ssh user@remote:/tmp/src/ dest\` |
| **Expected outcome** | Files downloaded to local destination |
| **Pass/fail criteria** | Local `dest\` matches remote source; exit code 0 |

### 6.6 Windows service mode

| Field | Value |
|-------|-------|
| **Description** | Install and run oc-rsync as a Windows service |
| **Command** | `sc.exe create oc-rsyncd binPath= "C:\path\to\oc-rsync.exe --daemon --config=C:\etc\oc-rsyncd.conf"`, then `sc.exe start oc-rsyncd` |
| **Expected outcome** | Service starts, listens on configured port, serves modules |
| **Pass/fail criteria** | `sc.exe query oc-rsyncd` shows RUNNING; `oc-rsync.exe rsync://localhost/` lists available modules |

---

## 7. Performance sanity checks

### 7.1 Large file throughput

| Field | Value |
|-------|-------|
| **Description** | Measure single-file transfer speed for regression detection |
| **Command** | `Measure-Command { & .\oc-rsync.exe -av bigfile_1gb.bin dest\ }` |
| **Expected outcome** | Throughput within 5% of previous release on same hardware |
| **Pass/fail criteria** | MB/s >= 95% of baseline from previous release. Record result for tracking. |

### 7.2 Many small files throughput

| Field | Value |
|-------|-------|
| **Description** | Measure per-file overhead with 10,000 small files |
| **Command** | `Measure-Command { & .\oc-rsync.exe -av small_files_10k\ dest\ }` |
| **Expected outcome** | Completes within 5% of previous release wall-clock time |
| **Pass/fail criteria** | Wall-clock time <= 105% of baseline. Record result for tracking. |

### 7.3 Comparison with upstream rsync

| Field | Value |
|-------|-------|
| **Description** | Verify oc-rsync is not slower than upstream rsync (MSYS2 build) |
| **Command** | Run both: `Measure-Command { rsync -av src/ dest_upstream/ }` and `Measure-Command { oc-rsync.exe -av src\ dest_ocrsync\ }` |
| **Expected outcome** | oc-rsync within 5% of upstream or faster |
| **Pass/fail criteria** | oc-rsync wall-clock time <= 1.05x upstream rsync time |

### 7.4 Delta transfer performance

| Field | Value |
|-------|-------|
| **Description** | Measure incremental sync with modified files (delta engine) |
| **Command** | Modify 10% of files in source, then `Measure-Command { & .\oc-rsync.exe -av src\ dest\ }` |
| **Expected outcome** | Only modified files transferred; throughput comparable to previous release |
| **Pass/fail criteria** | Transfer count matches modified file count; wall-clock time within 5% of baseline |

---

## 8. Edge cases

### 8.1 Unicode paths

| Field | Value |
|-------|-------|
| **Description** | Transfer files with Unicode names (CJK, emoji, combining characters) |
| **Command** | Create files named with Unicode (e.g., `data_\u{6587}\u{4EF6}.txt`, `report_\u{1F4CA}.csv`), then `oc-rsync.exe -av src\ dest\` |
| **Expected outcome** | All Unicode-named files transferred with names preserved |
| **Pass/fail criteria** | `Get-ChildItem dest\ -Recurse` shows all Unicode filenames intact; content matches |

### 8.2 Spaces in paths

| Field | Value |
|-------|-------|
| **Description** | Transfer files where source and destination paths contain spaces |
| **Command** | `oc-rsync.exe -av "C:\My Documents\src dir\" "D:\backup folder\dest dir\"` |
| **Expected outcome** | Transfer completes without path parsing errors |
| **Pass/fail criteria** | All files present in destination; no "file not found" or quoting errors |

### 8.3 UNC paths

| Field | Value |
|-------|-------|
| **Description** | Transfer to/from a UNC network path |
| **Command** | `oc-rsync.exe -av src\ "\\server\share\dest\"` |
| **Expected outcome** | Files transferred to network share |
| **Pass/fail criteria** | Files accessible at UNC destination; exit code 0 |

### 8.4 Drive-letter root sync

| Field | Value |
|-------|-------|
| **Description** | Sync from the root of a drive |
| **Command** | `oc-rsync.exe -av D:\ dest\` |
| **Expected outcome** | Root directory contents transferred (excluding system-protected files) |
| **Pass/fail criteria** | Non-protected files and directories transferred; permission errors logged but do not crash |

### 8.5 Read-only destination file overwrite

| Field | Value |
|-------|-------|
| **Description** | Overwrite a read-only file on the destination |
| **Command** | Set `dest\file.txt` to read-only, modify source, then `oc-rsync.exe -av src\ dest\` |
| **Expected outcome** | File updated despite read-only attribute |
| **Pass/fail criteria** | Destination file content matches updated source; exit code 0 |

### 8.6 Open file handles (in-use files)

| Field | Value |
|-------|-------|
| **Description** | Attempt to transfer a file that is locked by another process |
| **Command** | Open `src\locked.txt` exclusively in another process, then `oc-rsync.exe -av src\ dest\` |
| **Expected outcome** | Graceful error reported for locked file; other files transfer successfully |
| **Pass/fail criteria** | Error message mentions the locked file; remaining files transferred; exit code 23 (partial transfer) |

### 8.7 Symlinks without Developer Mode

| Field | Value |
|-------|-------|
| **Description** | Sync symlinks on a system without Developer Mode or `SeCreateSymbolicLinkPrivilege` |
| **Command** | Disable Developer Mode, then `oc-rsync.exe -avl src_with_symlinks\ dest\` |
| **Expected outcome** | Symlink creation fails gracefully with informative error |
| **Pass/fail criteria** | Error message mentions privilege requirement; non-symlink files still transferred; exit code 23 |

### 8.8 Console signal handling (Ctrl+C)

| Field | Value |
|-------|-------|
| **Description** | Interrupt a transfer mid-flight with Ctrl+C |
| **Command** | Start large transfer `oc-rsync.exe -av bigfile_10gb.bin dest\`, press Ctrl+C during transfer |
| **Expected outcome** | Graceful shutdown; partial temp file cleaned up |
| **Pass/fail criteria** | Process exits promptly (< 2s); no orphan temp files in destination |

---

## Recording results

For each release, record results in a table:

| Test | Version | Date | Result | Notes |
|------|---------|------|--------|-------|
| 1.1 | vX.Y.Z | YYYY-MM-DD | PASS/FAIL | ... |

Keep the completed checklist alongside the release artifacts in the
GitHub Release notes or as an internal tracking document.

## Cross-references

- `docs/user/windows-support-matrix.md` - feature support reference
- `docs/benchmarks/windows-throughput.md` - CI benchmark methodology
- `.github/workflows/ci.yml` - automated Windows CI cells
- `.github/workflows/_interop-windows.yml` - Windows interop smoke tests
- `crates/fast_io/src/iocp/` - IOCP implementation
- `crates/fast_io/src/iocp/config.rs` - IOCP thresholds and configuration
