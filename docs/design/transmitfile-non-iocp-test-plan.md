# TransmitFile non-IOCP test plan (WIN-S.11)

Tracking: WIN-S.11. Design-only; no code lands in this PR.

Related:
- `crates/fast_io/src/iocp/transmit_file.rs` - synchronous TransmitFile primitive
- `crates/fast_io/src/iocp/socket.rs:362-401` - `try_transmit_file_path` with WSASend fallback
- `docs/design/windows-transmitfile.md` - API survey and trait design
- `docs/design/windows-transmitfile-zerocopy.md` - integration plan and performance hypothesis
- `docs/design/win-s2-sendfile-transmitfile-audit.md` - gap analysis

## 1. Background

WPG-2 wired `TransmitFile` as a zero-copy file-to-socket primitive on
Windows. The implementation in `crates/fast_io/src/iocp/transmit_file.rs`
issues a **synchronous** `TransmitFile` call (null `OVERLAPPED`), meaning
the thread blocks until the kernel copies the entire file range into the
socket buffer. Despite the feature being named `transmitfile` and requiring
the `iocp` feature as a dependency, the actual `try_transmit_file` function
does NOT use IOCP completion notifications - it is a blocking call.

The IOCP path (overlapped I/O with completion port notification) is a
separate design tracked in `windows-transmitfile-zerocopy.md` step 3 and
has not been implemented. Today, the only wired path is the synchronous
(non-IOCP) variant.

### 1.1 Current integration state

The `transmitfile` Cargo feature gates compilation of the
`try_transmit_file` function. When enabled:

1. `IocpSocketWriter::try_transmit_file_path` calls `try_transmit_file`
   with the raw socket handle and file handle.
2. On success, the kernel transmits `length` bytes (all-or-nothing
   semantics) and returns the count.
3. On `ERROR_NOT_SUPPORTED`, the method falls back to a `Read` from the
   file handle into a caller-supplied buffer, followed by `send_async`
   (overlapped `WSASend`).
4. Other errors propagate directly.

### 1.2 Non-IOCP semantics

When `lpOverlapped` is null, `TransmitFile`:

- Blocks the calling thread until the entire transfer completes or fails.
- Returns `TRUE` (all bytes queued) or `FALSE` (error; call
  `WSAGetLastError`).
- Does not post to any completion port, even if the socket is associated
  with one.
- Respects `nNumberOfBytesPerSend` (64 KiB) for TCP segmentation.
- Does not support cancellation via `CancelIoEx` (the OVERLAPPED pointer
  is null, so there is no operation to cancel).

This is the path that needs independent validation.

## 2. Test scenarios

### 2.1 Correctness: byte-identical round-trip

Each scenario transmits a file over a localhost TCP loopback pair and
asserts byte-for-byte equality between source and received data.

| ID | Scenario | File size | Notes |
|---|---|---|---|
| T1 | Small file | 2 KiB | Below SENDFILE_THRESHOLD (64 KiB); verifies the primitive works at sub-threshold sizes where policy may skip it |
| T2 | Boundary file | 64 KiB | Exactly BYTES_PER_SEND; single kernel segment |
| T3 | Medium file | 1 MiB | Multiple 64 KiB segments; verifies segmentation loop integrity |
| T4 | Large file | 100 MiB | Exercises sustained throughput; detects TCP backpressure stalls |
| T5 | Near-DWORD-cap | 4 GiB - 1 | Maximum single-call size; exercises u32 boundary (CI-only, needs disk budget) |
| T6 | Exact DWORD cap | u32::MAX + 1 (4 GiB) | Verifies the InvalidInput rejection before any FFI call |
| T7 | Patterned content | 16 MiB | Repeating pattern (0x00-0xFF cycling) to catch byte-swap or alignment bugs |
| T8 | Random content | 8 MiB | Non-compressible random bytes to defeat any intermediate buffering optimization |

### 2.2 Error injection

| ID | Scenario | Injection method | Expected behavior |
|---|---|---|---|
| E1 | Socket closed mid-transfer | Receiver drops `TcpStream` after reading 1 KiB of a 1 MiB transfer | `try_transmit_file` returns `BrokenPipe` or `ConnectionReset` |
| E2 | Target file locked | Open source file with `FILE_SHARE_NONE` (exclusive lock) from another handle before transmit | `TransmitFile` returns an error (not UB); surface as `io::Error` |
| E3 | Non-socket destination | Pass a regular file HANDLE as the socket argument | Error (WSAENOTSOCK); existing test `transmit_file_non_socket_target_returns_error` covers this |
| E4 | Source on network share | Mount a test SMB share (or mock `ERROR_NOT_SUPPORTED` return) | `io::ErrorKind::Unsupported`; caller falls back to WSASend |
| E5 | Zero-length source | Empty file | Returns `Ok(0)` immediately (existing test covers this) |
| E6 | File pointer past EOF | `SetFilePointerEx` past end of a 1 KiB file, then transmit 1 KiB | Error or zero bytes; must not crash |
| E7 | Socket in TIME_WAIT | Connect, shutdown, then reuse the socket handle | Error; must not hang |

### 2.3 Fallback path validation

When `try_transmit_file` returns `Unsupported`, the `try_transmit_file_path`
method reads from the file into `fallback_buf` and sends via `send_async`.
This fallback must be tested independently:

| ID | Scenario | Validation |
|---|---|---|
| F1 | Force Unsupported via mock | Inject `ERROR_NOT_SUPPORTED` return; verify fallback produces correct bytes |
| F2 | Fallback buffer smaller than file | `fallback_buf` is 4 KiB, file is 64 KiB; verify outer loop iterates correctly |
| F3 | Fallback + partial read | Source file shorter than `length` (e.g., file is 3 KiB, length is 4 KiB); verify graceful short-read handling |

## 3. Parity verification: Linux sendfile equivalence

The non-IOCP TransmitFile path must produce byte-identical wire output to
the Linux `sendfile(2)` path for the same input file. This validates
cross-platform determinism.

### 3.1 Method

1. Generate a reference corpus: 1 KiB, 64 KiB, 1 MiB, 16 MiB files with
   fixed-seed PRNG content.
2. On Linux: capture wire bytes via `sendfile(2)` through a localhost TCP
   pair into a file.
3. On Windows: capture wire bytes via `TransmitFile` (non-IOCP) through
   a localhost TCP pair into a file.
4. Assert: captured payloads are byte-identical (excluding TCP framing
   handled by the OS).

### 3.2 Cross-platform test harness

A shared test in `crates/fast_io/tests/sendfile_parity.rs` abstracts the
platform dispatch:

```rust
fn send_file_bytes(source_path: &Path, length: u64) -> Vec<u8> {
    // Platform-dispatched: sendfile on Linux, TransmitFile on Windows,
    // read+write fallback elsewhere.
    // Returns the raw bytes received at the other end of a TCP loopback.
}
```

The test compares against a golden file generated by reading the source
directly (`std::fs::read`), which is the ground truth. Both platform
primitives must produce this exact output.

## 4. CI integration

### 4.1 Dedicated CI job: `windows-transmitfile-no-iocp`

A new CI matrix entry exercises TransmitFile without the full IOCP
completion-port machinery active. This validates the synchronous path in
isolation.

| Attribute | Value |
|---|---|
| `runs-on` | `windows-latest` |
| Feature flags | `--features transmitfile --no-default-features -p fast_io` |
| Test filter | `-E 'test(/transmit_file/) & package(fast_io)'` |
| Purpose | Validate synchronous TransmitFile independently of IOCP pump |

### 4.2 IOCP-disabled configuration

The `transmitfile` feature currently implies `iocp` (`transmitfile = ["iocp"]`).
To test TransmitFile in true non-IOCP mode, two options:

**Option A (preferred): Decouple the feature gate.**

Split `transmitfile` into `transmitfile-sync` (no IOCP dependency) and
`transmitfile` (full IOCP path, future). The `try_transmit_file` function
itself does not use IOCP - it passes null OVERLAPPED. The IOCP dependency
in the feature is structural (the file lives in `src/iocp/`), not
functional.

```toml
# Cargo.toml change
transmitfile-sync = []  # Synchronous TransmitFile, no IOCP dependency
transmitfile = ["iocp", "transmitfile-sync"]  # Full overlapped path (future)
```

**Option B: Conditional compilation within the module.**

Keep the current feature structure. Add `#[cfg(test)]` paths that bypass
IOCP setup and call `try_transmit_file` directly against stdlib
`TcpStream` handles (which are overlapped-capable by default without a
completion port).

Option A is preferred because it makes the separation explicit in CI and
avoids `#[cfg(test)]`-only code paths that diverge from production.

### 4.3 Test execution constraints

- **Disk budget:** The 4 GiB boundary test (T5) requires 8+ GiB free disk
  (source + received buffer). Gate behind an env var
  (`OC_RSYNC_LARGE_FILE_TESTS=1`) or skip in PR CI, run only in nightly.
- **Timeout:** Large file tests must complete within 60 seconds on CI
  runners. Localhost TCP throughput on Windows CI is typically 2-4 Gbps,
  so 100 MiB completes in under 1 second.
- **Parallelism:** Each test binds its own ephemeral port. No shared state.
  Tests are safe to run in parallel via nextest.

## 5. Performance baseline

### 5.1 Measurement goal

Quantify the throughput advantage of synchronous `TransmitFile` over the
equivalent `ReadFile` + `WSASend` loop for various file sizes. This
establishes whether the non-IOCP TransmitFile path justifies its
complexity relative to the plain buffered fallback.

### 5.2 Benchmark design

```
Benchmark: transmitfile_vs_readwrite
  Variants:
    A) try_transmit_file (synchronous, null OVERLAPPED)
    B) read_exact + send_async (64 KiB buffer, loop until length exhausted)
  File sizes: 64 KiB, 1 MiB, 16 MiB, 100 MiB, 1 GiB
  Socket: localhost TCP loopback
  Iterations: 20 per variant per size (warm cache; first iteration is warmup)
  Metrics: wall time (ms), CPU user time, bytes/sec
```

### 5.3 Expected results

Based on profiling data from #2130:

| File size | TransmitFile advantage | Rationale |
|---|---|---|
| 64 KiB | ~0% (noise) | Syscall overhead dominates at small sizes |
| 1 MiB | 5-10% | One fewer memcpy per 64 KiB chunk adds up |
| 16 MiB | 15-25% | Sustained DMA advantage visible |
| 100 MiB | 25-35% | Eliminates ~22% CPU in memcpy (profile #2130) |
| 1 GiB | 30-50% | Full throughput advantage on warm cache |

If the measured advantage at 100 MiB is below 10%, the non-IOCP path may
not justify enablement by default at that file size. The threshold should
be raised from the current 64 KiB `SENDFILE_THRESHOLD` to the measured
break-even point.

### 5.4 Regression tracking

Add the benchmark to `scripts/benchmark.sh` under a `--windows-io` flag.
Track results in CI artifacts for trend detection. A 20% regression from
the baseline on the 100 MiB variant triggers investigation.

## 6. Implementation order

| Step | PR | Description |
|---|---|---|
| 1 | WIN-S.11a | Add T1-T4, T7-T8 correctness tests in `crates/fast_io/tests/transmitfile_sync.rs` |
| 2 | WIN-S.11b | Add E1-E7 error injection tests |
| 3 | WIN-S.11c | Add F1-F3 fallback path tests; requires mock or cfg-gated error injection |
| 4 | WIN-S.11d | Cross-platform parity harness (`sendfile_parity.rs`) |
| 5 | WIN-S.11e | Decouple `transmitfile-sync` feature gate (Option A) |
| 6 | WIN-S.11f | CI matrix entry `windows-transmitfile-no-iocp` |
| 7 | WIN-S.11g | Performance benchmark harness and baseline capture |

Steps 1-3 are independent and can be parallelized. Step 4 depends on 1
(needs the test infrastructure). Steps 5-6 are structural and can land in
any order relative to the tests. Step 7 is standalone.

## 7. Open questions

1. **Should the 4 GiB boundary test run in PR CI?** The disk cost is
   significant on GitHub-hosted runners (14 GB total). Recommendation:
   nightly-only, gated by env var.
2. **Is the `iocp` feature dependency justified for synchronous-only use?**
   The `transmit_file.rs` module lives under `src/iocp/` and the feature
   implies `iocp`, but the synchronous call uses neither completion ports
   nor OVERLAPPED structs. Decoupling (Option A in section 4.2) is cleaner.
3. **Should the fallback path use `WriteFile` directly instead of
   `send_async` (which goes through IOCP)?** In a true non-IOCP build, the
   fallback should use a synchronous `send()` or `WSASend` without
   OVERLAPPED. This needs investigation during WIN-S.11c.
