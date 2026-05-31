# Windows Feature Flags and Test Coverage Audit (WSD-2)

Date: 2026-06-01

## 1. Inventory of Windows-Relevant Feature Flags

### 1.1 `iocp` - I/O Completion Ports

| Property | Value |
|----------|-------|
| Owner crate | `fast_io` |
| Forwarded via | `bin` -> `transfer/iocp` -> `fast_io/iocp`; `bin` -> `fast_io/iocp` |
| Default | Yes (in `fast_io` defaults and workspace `bin` defaults) |
| Gates | Windows overlapped async file reads/writes via `CreateIoCompletionPort`, `GetQueuedCompletionStatus`, file writer (`IocpDiskBatch`), file reader, completion port, pump, overlapped helpers, socket WSARecv/WSASend |
| Cfg pattern | `#[cfg(all(target_os = "windows", feature = "iocp"))]` |
| Source modules | `crates/fast_io/src/iocp/` (mod.rs, completion_port.rs, config.rs, disk_batch/, error.rs, file_factory.rs, file_reader.rs, file_writer.rs, overlapped.rs, pump.rs, socket.rs) |
| Consumer | `crates/transfer/src/disk_commit/writer.rs` dispatches `Writer::Iocp` variant |

### 1.2 `transmitfile` - Windows TransmitFile Zero-Copy

| Property | Value |
|----------|-------|
| Owner crate | `fast_io` |
| Forwarded via | Not forwarded from workspace root (opt-in only at `fast_io` level) |
| Default | No |
| Gates | `TransmitFile()` zero-copy file-to-socket primitive; requires `iocp` feature |
| Cfg pattern | `#[cfg(all(target_os = "windows", feature = "transmitfile"))]` |
| Source modules | `crates/fast_io/src/iocp/transmit_file.rs`, integration in `socket.rs` |
| Consumer | Not yet wired into production transfer path |

### 1.3 `acl` - Access Control Lists (Windows DACL)

| Property | Value |
|----------|-------|
| Owner crate | `metadata` |
| Forwarded via | `bin` -> `cli/acl` -> `core/acl` -> `metadata/acl` + `engine/acl` + `transfer/acl` |
| Default | Yes (workspace `bin` defaults) |
| Gates | Windows DACL via Win32 `GetSecurityInfo`/`SetSecurityInfo`; entire `acl_windows` module |
| Cfg pattern | `#[cfg(all(feature = "acl", windows))]` |
| Source modules | `crates/metadata/src/acl_windows/` (mod.rs, dacl.rs, sddl.rs, posix_map.rs, sync.rs, xattr.rs, common.rs, tests/) |
| Consumer | `engine`, `transfer`, `core`, `cli` all gate ACL logic on `#[cfg(all(any(unix, windows), feature = "acl"))]` |

### 1.4 `xattr` - Extended Attributes (Windows NTFS ADS)

| Property | Value |
|----------|-------|
| Owner crate | `metadata` |
| Forwarded via | `bin` -> `cli/xattr` -> `core/xattr` -> `transfer/xattr`; engine gets xattr via target-dep |
| Default | Yes (workspace `bin` defaults) |
| Gates | Windows NTFS Alternate Data Streams backend (`xattr_windows` module) |
| Cfg pattern | `#[cfg(all(feature = "xattr", windows))]` |
| Source modules | `crates/metadata/src/xattr_windows.rs`, `crates/metadata/src/xattr.rs` |
| Consumer | Core, engine, transfer for metadata preservation during sync |

### 1.5 `async` - Tokio Async Runtime

| Property | Value |
|----------|-------|
| Owner crate | `engine`, `transfer`, `daemon`, `core` |
| Forwarded via | `bin` -> `daemon/async` + `core/async` -> `engine/async` + `transfer/async` |
| Default | Yes (workspace `bin` defaults) |
| Gates | Tokio-based async I/O in engine/transfer/daemon (platform-neutral) |
| Windows relevance | Cross-platform; enables async pipeline used by IOCP path consumers |

### 1.6 `concurrent-sessions` - Daemon Concurrent Session Tracking

| Property | Value |
|----------|-------|
| Owner crate | `daemon` |
| Forwarded via | Not in workspace defaults |
| Default | No |
| Gates | `DashMap`-backed session state for multi-client daemons |
| Windows relevance | Cross-platform; daemon compiles on Windows (though `--daemon` CLI is refused) |

### 1.7 `daemon-tls` - Native TLS Daemon Termination

| Property | Value |
|----------|-------|
| Owner crate | `daemon` |
| Forwarded via | `bin` -> `daemon/daemon-tls` |
| Default | No |
| Gates | rustls-based TLS listener for the rsync daemon |
| Windows relevance | Cross-platform; compiles on Windows |

### 1.8 `dg-stress` - Parallel Applier Stress Test

| Property | Value |
|----------|-------|
| Owner crate | `engine` |
| Default | No |
| Gates | High-iteration concurrency stress test for `SlotData`/`BarrierState` |
| Windows relevance | Cross-platform test; explicitly run on Windows in CI |

### 1.9 `embedded-ssh` - Pure-Rust SSH Transport

| Property | Value |
|----------|-------|
| Owner crate | `rsync_io` |
| Forwarded via | `bin` -> `core/embedded-ssh` -> `rsync_io/embedded-ssh` |
| Default | No |
| Gates | russh-based SSH transport eliminating external `ssh` binary dependency |
| Windows relevance | Provides SSH on Windows where OpenSSH may not be installed |

### 1.10 `openssl` / `openssl-vendored` - Hardware-Accelerated Checksums

| Property | Value |
|----------|-------|
| Owner crate | `checksums` |
| Forwarded via | `bin` -> `checksums/openssl`; auto-enabled on `cfg(all(unix, not(target_env = "musl")))` |
| Default | No on Windows (only auto-enabled on non-musl Unix) |
| Gates | OpenSSL EVP MD4/MD5 for `--checksum` mode |
| Windows relevance | Not default on Windows; pure-Rust md-5/md4 used instead. The `openssl-vendored` variant can be explicitly enabled |

### 1.11 Platform-Specific Crates (No Feature Gates)

| Crate | Windows Behavior |
|-------|-----------------|
| `platform` | No feature flags; Windows code via `#[cfg(windows)]` - signal handling, name resolution (LookupAccountNameW, NetLocalGroupGetMembers), privilege checks (LogonUserW), Windows Service dispatcher |
| `windows-gnu-eh` | Shim crate for DWARF unwinding on `x86_64-pc-windows-gnu`; no features |
| `checksums` | No Windows-specific features; `sha1`/`sha2`/`md-5` compiled without `asm` feature on `cfg(not(unix))` (NASM not available on MSVC) |

## 2. CI Coverage Matrix

### 2.1 Windows CI Jobs

| CI Job | Runner | Features Tested | Crates Tested | Required? |
|--------|--------|-----------------|---------------|-----------|
| `Windows (stable/beta/nightly)` | `windows-latest` | `--all-features` | `core`, `engine`, `cli` | Yes (stable) |
| `Windows IOCP (--features iocp)` | `windows-latest` | `iocp` isolated | `fast_io`, `transfer` | Yes |
| `Windows ACL/xattr` | `windows-latest` | `acl`, `xattr` | `metadata`, workspace filter | Yes |
| `Windows GNU cross-check` | `ubuntu-latest` | Default (check only) | Workspace | Yes |
| `DG-3 stress (windows-latest)` | `windows-latest` | `dg-stress` | `engine` | No (non-required) |
| `interop (Windows, best-effort)` | `windows-latest` | Default (release) | Binary smoke | No (continue-on-error) |
| `Feature: async (windows-latest)` | `windows-latest` | `async` | `daemon`, `core`, `protocol`, `engine` | Yes |
| `Feature: tracing (windows-latest)` | `windows-latest` | `tracing` | `daemon`, `core`, `engine` | Yes |
| `Feature: serde (windows-latest)` | `windows-latest` | `serde` | `logging`, `protocol`, `flist` | Yes |
| `Feature: concurrent-sessions (windows-latest)` | `windows-latest` | `concurrent-sessions` | `daemon` | Yes |
| `Feature: daemon-tls (windows-latest)` | `windows-latest` | `daemon-tls` | `daemon` | Yes |

### 2.2 Per-Feature Test Coverage

| Feature | CI on Windows? | Unit Tests | Integration Tests | Coverage |
|---------|---------------|------------|-------------------|----------|
| `iocp` | Yes (dedicated job) | Yes - `fast_io/src/iocp/disk_batch/tests.rs` | Yes - 4 files (25 tests): completion port, disk full, high concurrency stress, partial write | **Full** |
| `transmitfile` | No | No dedicated tests | No | **None** |
| `acl` (Windows) | Yes (dedicated job) | Yes - 37 tests across dacl, sddl, posix_map, sync, xattr modules | Yes - workspace filter `test(acl)` | **Full** |
| `xattr` (Windows) | Yes (dedicated job) | Yes - in xattr_windows module | Yes - workspace filter `test(xattr) \| test(ads) \| test(stream)` | **Full** |
| `async` | Yes (cross-OS matrix) | Yes | No Windows-specific | **Partial** |
| `concurrent-sessions` | Yes (cross-OS matrix) | Yes | No Windows-specific | **Partial** |
| `daemon-tls` | Yes (cross-OS matrix) | Yes | No Windows-specific | **Partial** |
| `dg-stress` | Yes (non-required) | Stress test only | N/A | **Full** |
| `embedded-ssh` | No Windows job | No Windows tests | No | **None** |
| `openssl` / `openssl-vendored` | No (Linux-only job) | N/A on Windows | N/A | **N/A** (not default on Windows) |

### 2.3 NTFS / Platform Tests (No Feature Gate)

| Test File | Tests | Run in CI? |
|-----------|-------|-----------|
| `fast_io/tests/ntfs_edge_cases.rs` | Long paths, case-insensitive, reparse points, file attributes | Yes (via `windows-iocp` job `--no-default-features --features iocp` and `windows-test` `--all-features`) |
| `fast_io/tests/win_tmpfile_delete_on_close.rs` | DELETE_ON_CLOSE handle semantics | Yes (same jobs) |
| `platform/src/windows_service.rs` | Service dispatcher, event log stubs | Yes (via `windows-test` `core` dep) |

## 3. Identified Gaps

### 3.1 Features Without Windows-Specific Tests

| Feature | Gap Description | Severity |
|---------|-----------------|----------|
| `transmitfile` | Feature exists in `fast_io` with full source module but zero tests and no CI coverage. Not exercised on any platform. | **High** - dead code risk |
| `embedded-ssh` | No Windows CI job tests this feature. Windows is arguably the primary beneficiary (no system `ssh`). | **Medium** - cross-platform feature, likely works, but untested on target platform |

### 3.2 Features Tested on Linux but Not Windows

| Feature | Linux CI | Windows CI | Gap |
|---------|----------|-----------|-----|
| `io_uring` | Yes (dedicated job) | N/A | None (Linux-only by design) |
| `copy_file_range` | Yes (dedicated job) | N/A | None (Linux-only by design) |
| `landlock` | Yes (dedicated job) | N/A | None (Linux-only by design) |
| `openssl` | Yes (dedicated job) | No | Intentional - not default on Windows |
| `zlib-ng` | Yes (Linux-only row) | No | **Low** - C SIMD library; should compile on Windows but untested |
| `zlib-rs` | Yes (Linux-only row) | No | **Low** - pure Rust; should work cross-platform |
| `parallel` | Yes (Linux-only row) | No explicit row | **Low** - covered implicitly via `--all-features` in `windows-test` |
| `flat-flist` | Yes (Linux-only row) | No | **Low** - pure data structure; no platform-specific code |
| `no-default-features` | Yes (Linux-only row) | No | **Medium** - compilation without defaults not verified on Windows |
| `default-features` | Yes (Linux-only row) | No explicit row | **Low** - the `windows-test` job uses `--all-features` which is a superset |

### 3.3 Feature Combinations Not Exercised

| Combination | Description | Risk |
|-------------|-------------|------|
| `iocp` + `transmitfile` | TransmitFile requires iocp; never tested together | **High** - untested code path |
| `iocp` + `async` | Async runtime + IOCP batching interaction | **Low** - both in defaults; tested via `windows-test --all-features` |
| `acl` + `xattr` + `iocp` | Full metadata preservation with async I/O | **Low** - tested by `windows-acl-xattr` + `windows-test` combined coverage |
| `no-default-features` on Windows | Whether the crate compiles with everything off on Windows | **Medium** - never checked on Windows runner |
| `embedded-ssh` on Windows | Russh transport on the platform that most needs it | **Medium** - likely works but unverified |
| `openssl-vendored` on Windows | Statically-linked OpenSSL for Windows checksum acceleration | **Low** - explicit opt-in; user would test before deploying |

### 3.4 Structural Observations

1. **Windows test scope is narrow.** The main `windows-test` job only tests `core`, `engine`, and `cli` crates. The `fast_io`, `transfer`, `protocol`, `filters`, `checksums`, `daemon`, `rsync_io` crates are not directly tested on Windows outside their specific jobs.

2. **Daemon refused on Windows at CLI level.** The `--daemon` flag is rejected on Windows (`crates/cli/src/frontend/server/daemon.rs`), so daemon feature testing on Windows (concurrent-sessions, daemon-tls) verifies compilation and unit tests but not integration behavior.

3. **`platform` crate has no Windows test job.** Windows service dispatcher, name resolution, privilege checks, and group membership APIs are compiled (via `core` dependency) but the `platform` crate itself is not named in any Windows CI step. Test coverage depends on its consumers exercising the paths.

4. **Feature-flag test workflow (`_test-features.yml`) runs 5 cross-OS rows on Windows.** These cover `async`, `tracing`, `serde`, `concurrent-sessions`, and `daemon-tls`. The remaining 13 Linux-only rows (including `no-default-features`, `default-features`, `compression`, `parallel`, `flat-flist`) have no Windows equivalent.

5. **Interop is best-effort.** The Windows interop job runs with `continue-on-error: true` and is not a required check. Failures do not block merges.

## 4. Recommendations

| Priority | Action | Addresses |
|----------|--------|-----------|
| P1 | Add `transmitfile` to the `windows-iocp` CI job or remove the feature if not shipping | Gap 3.1 |
| P2 | Add `embedded-ssh` build + test row on Windows | Gap 3.1 |
| P2 | Add `no-default-features` compilation check on Windows (in `_test-features.yml` cross-OS matrix or `windows-test`) | Gap 3.2, 3.3 |
| P3 | Promote Windows interop to required once baseline parity is green | Gap 3.4 |
| P3 | Add `platform` crate explicitly to the `windows-test` step | Gap 3.4 |
| P3 | Add `zlib-ng` and `zlib-rs` to cross-OS feature matrix | Gap 3.2 |
