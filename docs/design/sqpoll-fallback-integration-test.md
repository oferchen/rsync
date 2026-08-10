# SQP-5: Integration test for SQPOLL graceful fallback in restricted containers

Tracking: SQP-5 (#3299). Parent tracker: SQP-1..6 (#3295-#3300).

Related artefacts:

- `docs/audit/sqpoll-capability-requirements.md` (SQP-1) - per-kernel-version
  capability matrix and failure mode catalogue.
- `docs/design/sqpoll-capability-error-message.md` (SQP-3) - explicit log
  emission when SQPOLL falls back.
- `crates/fast_io/src/io_uring/config.rs:357-387` - `build_ring()` fallback
  implementation.
- `crates/fast_io/tests/io_uring_probe_fallback.rs` - existing unit-level
  probe and fallback assertions.
- `.github/workflows/iouring-kernel-compat.yml` - io_uring kernel tier CI.

## 1. Goal

Validate end-to-end that a transfer completes correctly and that SQPOLL
degrades gracefully when the process lacks `CAP_SYS_NICE`. The test must:

1. Simulate a restricted-capability environment (no `CAP_SYS_NICE`).
2. Request SQPOLL via `IoUringConfig { sqpoll: true, .. }`.
3. Assert that the fallback occurred (`sqpoll_fell_back() == true`).
4. Assert that a non-SQPOLL io_uring ring was successfully constructed.
5. Run a real file transfer through the ring and verify byte-correct output.

## 2. Simulating restricted capabilities

### 2.1 Options considered

| Approach | Pros | Cons | Verdict |
|----------|------|------|---------|
| **A. `prctl(PR_SET_NO_NEW_PRIVS)` + `prctl(PR_CAPBSET_DROP)`** | In-process, no external dependency, works in CI without docker/podman | `CAP_BSET` drop only affects new execs; active effective set needs `capset(2)` or the `caps` crate | **Selected** |
| **B. `capset(2)` via libc** | Direct effective-set manipulation in-process | Requires careful bitmask construction; raw `unsafe` | Fallback if `caps` is too heavy |
| **C. rootless podman container** | Realistic production scenario | Requires podman in CI; adds 30+ seconds container overhead; not available on macOS/Windows runners | Future extension (CI-only) |
| **D. seccomp BPF to block `io_uring_setup` with SQPOLL flag** | Precise; would test the exact kernel rejection path | Seccomp filters block all `io_uring_setup` variants, not just SQPOLL; overly aggressive | Rejected |
| **E. Environment variable (`OC_RSYNC_DISABLE_IOURING`)** | Simple | Does not test SQPOLL fallback - disables all io_uring | Rejected |

### 2.2 Selected approach: capability drop via `prctl`

The test uses `prctl(PR_SET_NO_NEW_PRIVS, 1)` followed by clearing
`CAP_SYS_NICE` from the effective, permitted, and inheritable sets via
`capset(2)`. This is a permanent, irreversible change for the calling
thread's process - acceptable in a nextest runner because each test binary
is a separate process.

Fallback: If capability manipulation fails (non-root on kernels that
disallow even reading the bitmask), the test skips gracefully with an
`eprintln!` message. This avoids flaky failures on unusual CI
configurations.

### 2.3 Implementation sketch

```rust
/// Drops CAP_SYS_NICE from the current process.
/// Returns `true` if the drop succeeded, `false` if it was not possible
/// (already unprivileged or kernel rejected the capset).
fn drop_cap_sys_nice() -> bool {
    // On Linux, use libc::prctl + libc::syscall(SYS_capset) or the `caps` crate.
    // On non-Linux, return false (test skips).
    #[cfg(not(target_os = "linux"))]
    return false;

    #[cfg(target_os = "linux")]
    {
        // PR_SET_NO_NEW_PRIVS prevents regaining capabilities.
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

        // Read current capabilities, clear CAP_SYS_NICE, write back.
        // CAP_SYS_NICE = 23 (include/uapi/linux/capability.h)
        // ... (raw capset or via `caps` crate)
    }
}
```

Alternative: The existing CI runners (ubuntu-22.04, ubuntu-24.04) already
run as an unprivileged user without `CAP_SYS_NICE`. The test can simply
probe whether `CAP_SYS_NICE` is absent from the effective set and skip on
privileged hosts. This is simpler and matches reality: the test passes on
the exact environment where the fallback occurs.

### 2.4 Recommended strategy for CI

On standard GitHub Actions runners the test process does NOT have
`CAP_SYS_NICE`. Therefore:

- **Primary path:** Check if `CAP_SYS_NICE` is in the effective set. If
  absent (the common CI case), proceed with the test. The kernel will
  reject SQPOLL with `EPERM` and the fallback fires naturally.
- **Secondary path:** If `CAP_SYS_NICE` IS present (root/privileged host),
  actively drop it via `capset(2)` before the test body. If the drop fails,
  skip the test with a diagnostic message.

This two-path strategy means the test is effective on CI without requiring
containers, and gracefully skips on hosts where capability manipulation is
unavailable.

## 3. Assertions proving the fallback

### 3.1 Observable signals

| Signal | API | What it proves |
|--------|-----|----------------|
| `sqpoll_fell_back() == true` | `fast_io::sqpoll_fell_back()` | `build_ring()` attempted SQPOLL, got `EPERM`, and recorded the fallback. |
| Ring construction succeeded | `IoUringConfig::build_ring().is_ok()` | The non-SQPOLL ring was created as a replacement. |
| Transfer byte-correctness | `sha256(src) == sha256(dst)` | The fallback ring services I/O correctly end-to-end. |
| No panic or abort | Test completes | The fallback path has no unwinding hazard. |

### 3.2 Optional diagnostic signals (SQP-3 dependency)

Once SQP-3 lands the explicit log emission:

| Signal | How to capture | What it proves |
|--------|----------------|----------------|
| Warning log line emitted | Capture `debug_log!` output or structured log sink | Operator would see the degrade in production. |
| `io_uring_capability_matrix()` reports SQPOLL unavailable | Query after transfer | `--io-uring-status` output is correct post-fallback. |

These are stretch assertions - the core test does not depend on SQP-3.

## 4. Test structure

### 4.1 File location

```
crates/fast_io/tests/sqpoll_fallback_restricted.rs
```

Placed alongside existing `sqpoll_mlock_fault_injection.rs` and
`io_uring_probe_fallback.rs`.

### 4.2 Test functions

```rust
//! Integration test: SQPOLL graceful fallback under restricted capabilities.
//!
//! Verifies that requesting SQPOLL on a process without CAP_SYS_NICE
//! transparently falls back to a regular io_uring ring, and that file I/O
//! through the fallback ring produces byte-correct results.
//!
//! Linux-only: SQPOLL is a Linux io_uring feature.

#![cfg(target_os = "linux")]

#[test]
fn sqpoll_fallback_builds_regular_ring_without_cap_sys_nice() {
    // 1. Skip if io_uring is entirely unavailable (kernel < 5.6, seccomp).
    // 2. Verify or establish that CAP_SYS_NICE is absent.
    // 3. Reset SQPOLL_FALLBACK to false (process-global atomic).
    // 4. Call build_ring() with sqpoll: true.
    // 5. Assert build_ring() returns Ok(_).
    // 6. Assert sqpoll_fell_back() == true.
}

#[test]
fn sqpoll_fallback_ring_completes_file_write_read_cycle() {
    // 1. Same capability setup as above.
    // 2. Build a ring with sqpoll: true (falls back).
    // 3. Write 1 MiB of pseudorandom data through the ring.
    // 4. Read it back.
    // 5. Assert byte-for-byte equality.
    // 6. Assert sqpoll_fell_back() == true.
}

#[test]
fn sqpoll_fallback_does_not_panic_under_concurrent_load() {
    // 1. Same capability setup.
    // 2. Spawn 4 threads, each building a ring with sqpoll: true.
    // 3. Each thread does a 256 KiB write+read cycle.
    // 4. Join all threads - no panics.
    // 5. Assert sqpoll_fell_back() == true.
}
```

### 4.3 Skip logic

```rust
fn should_skip() -> bool {
    if !fast_io::is_io_uring_available() {
        eprintln!("skipping: io_uring unavailable (kernel < 5.6 or seccomp)");
        return true;
    }
    if has_cap_sys_nice() && !drop_cap_sys_nice() {
        eprintln!("skipping: CAP_SYS_NICE present and could not be dropped");
        return true;
    }
    false
}
```

## 5. CI integration

### 5.1 Existing workflow coverage

The `iouring-kernel-compat.yml` workflow already runs `fast_io` tests
filtered by `test(sqpoll)` on ubuntu-22.04 (kernel ~5.15) and
ubuntu-24.04 (kernel ~6.8). Both runners lack `CAP_SYS_NICE`, so the new
test will exercise the natural fallback path without any special
configuration.

The filter expression in that workflow:

```
-E 'test(io_uring) | test(iouring) | ... | test(sqpoll) | ...'
```

already matches `sqpoll_fallback_*` by prefix, so no workflow change is
needed.

### 5.2 Main CI (`ci.yml`)

The main `nextest` job runs all tests with `--workspace --all-features`.
The new test file compiles and runs on Linux runners only (`#![cfg(target_os = "linux")]`).
On macOS and Windows CI cells it is excluded at compile time.

### 5.3 Future: podman-based container test (non-blocking)

A separate optional CI job could validate the fallback inside a rootless
podman container with an explicit seccomp profile that allows `io_uring_setup`
but denies `CAP_SYS_NICE`:

```yaml
- name: SQPOLL fallback in rootless container
  run: |
    podman run --rm --cap-drop=SYS_NICE \
      -v ${{ github.workspace }}:/src:ro \
      rust:latest \
      bash -c "cd /src && cargo nextest run -p fast_io \
        -E 'test(sqpoll_fallback)' --all-features"
```

This is a stretch goal - not required for SQP-5 completion. The in-process
capability check is sufficient for the first iteration.

## 6. Dependencies and ordering

| Dependency | Required? | Status |
|------------|-----------|--------|
| SQP-1 (audit) | Informational | Completed |
| SQP-3 (error message) | Optional (enables log assertion) | Design complete |
| io_uring available on CI runner | Yes | ubuntu-22.04+ provides it |
| `sqpoll_fell_back()` public API | Yes | Already exported from `fast_io` |
| `SQPOLL_FALLBACK` reset mechanism | Yes (for test isolation) | Needs internal `reset_sqpoll_fallback()` test helper or `#[cfg(test)]` setter |

### 6.1 Required internal change

The `SQPOLL_FALLBACK` atomic is currently set-only (no reset). For test
isolation across multiple tests in the same binary, a `#[cfg(test)]`
reset function is needed:

```rust
// In crates/fast_io/src/io_uring/config.rs
#[cfg(test)]
pub(crate) fn reset_sqpoll_fallback() {
    SQPOLL_FALLBACK.store(false, Ordering::Relaxed);
}
```

Integration tests (separate binaries) do not share process state so they
do not strictly need this - each test binary starts with
`SQPOLL_FALLBACK = false`. But it is good hygiene for co-located unit
tests.

## 7. Success criteria

| Criterion | Measurement |
|-----------|-------------|
| Test passes on ubuntu-22.04 (kernel ~5.15) | CI green |
| Test passes on ubuntu-24.04 (kernel ~6.8) | CI green |
| Test skips gracefully on macOS/Windows | Compile-time `#![cfg(target_os = "linux")]` |
| Test skips gracefully when io_uring unavailable | Runtime skip with diagnostic |
| Fallback atomic correctly reflects SQPOLL rejection | `assert!(sqpoll_fell_back())` |
| Data integrity preserved through fallback ring | SHA-256 comparison of written vs read bytes |
| No test flakiness from process-global state | Each test function is in its own binary (nextest default) or uses the reset helper |

## 8. Non-goals

- Testing the mmap+SQPOLL defensive disable path. That is covered by
  existing tests in `config.rs` (`build_ring_sqpoll_with_mmap_basis_*`).
- Testing `WiredBasisWindow` mlock downgrades. Covered by
  `sqpoll_mlock_fault_injection.rs`.
- Adding a new CI workflow. The existing `iouring-kernel-compat.yml` and
  main `ci.yml` nextest jobs cover the new test file.
- Benchmarking SQPOLL vs non-SQPOLL throughput. Out of scope - belongs to
  `iouring-sqpoll-bench-plan.md`.
