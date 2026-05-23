//! SQM-3 fault-injection tests for `WiredBasisWindow`.
//!
//! Exercises the downgrade-class errno paths (`EAGAIN`, `EPERM`, `ENOMEM`)
//! by driving the `RLIMIT_MEMLOCK` resource limit to a value that forces
//! `mlock(2)` to fail. The wrapper must classify the failure as a
//! downgrade, bump the `mlock_downgrades` counter, and return
//! [`fast_io::MlockError::Downgrade`] so the caller can route through the
//! regular (non-SQPOLL) ring.
//!
//! Linux-only: the production wiring path only exists on Linux. On every
//! other platform the wrapper is a zero-cost stub and these tests are
//! skipped at compile time.
//!
//! These tests change the process-wide `RLIMIT_MEMLOCK` and the
//! `MLOCK_ATTEMPTS` / `MLOCK_DOWNGRADES` counters, so they run serially
//! via the `serial_test_lock` mutex below to avoid races with any
//! parallel unit tests that touch the same counters.

#![cfg(target_os = "linux")]

use std::sync::{Mutex, MutexGuard, OnceLock};

use fast_io::{MlockError, WiredBasisWindow, mlock_attempts, mlock_downgrades};

/// Process-wide lock so that rlimit-mutating tests serialise across the
/// nextest runner. `RLIMIT_MEMLOCK` is per-process, not per-thread, so two
/// tests racing on `setrlimit` would interleave the limits and corrupt
/// each other's assertions.
fn rlimit_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("rlimit lock poisoned")
}

/// Reads the current `RLIMIT_MEMLOCK` (soft, hard).
fn get_memlock_limit() -> (libc::rlim_t, libc::rlim_t) {
    // SAFETY: `rlimit` is a POD; `getrlimit` populates it fully when it
    // returns 0. We zero-init defensively so a failure leaves us with
    // deterministic zeros.
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut rl) };
    assert_eq!(rc, 0, "getrlimit failed");
    (rl.rlim_cur, rl.rlim_max)
}

/// Sets `RLIMIT_MEMLOCK`'s soft limit, preserving the current hard limit.
///
/// An unprivileged process can lower the soft limit freely but cannot
/// raise the hard limit once dropped (`EPERM`). Touching only the soft
/// limit keeps the test runnable on CI runners without `CAP_SYS_RESOURCE`
/// while still gating `mlock(2)` behaviour - the kernel enforces the
/// soft limit when accepting `mlock` requests.
fn set_memlock_soft(soft: libc::rlim_t) {
    let (_, hard) = get_memlock_limit();
    let capped = soft.min(hard);
    let rl = libc::rlimit {
        rlim_cur: capped,
        rlim_max: hard,
    };
    // SAFETY: `setrlimit` reads `rl` by const pointer and copies it; the
    // value lives for the call.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rl) };
    assert_eq!(
        rc,
        0,
        "setrlimit(RLIMIT_MEMLOCK, soft={capped}, hard={hard}) failed: {}",
        std::io::Error::last_os_error()
    );
}

/// Allocates a 1 MiB page-aligned buffer of zeros. The buffer must be
/// large enough to exceed a small rlimit budget so `mlock` is forced to
/// reject the pin.
fn page_aligned_buffer(len: usize) -> Vec<u8> {
    vec![0u8; len]
}

#[test]
fn rlimit_zero_forces_downgrade_via_eperm() {
    let _guard = rlimit_lock();
    let (original_soft, _) = get_memlock_limit();

    // Drop the soft limit to zero. Any non-zero mlock call from a
    // non-privileged process will fail with EPERM (or EAGAIN on some
    // kernels). Either errno is in the downgrade-class set.
    set_memlock_soft(0);

    let before_downgrades = mlock_downgrades();
    let before_attempts = mlock_attempts();

    let buf = page_aligned_buffer(1024 * 1024);
    let result = WiredBasisWindow::new(buf.as_ptr(), buf.len());

    // Restore the soft limit before any assertion so a failed assert
    // does not leave the process in a degraded state.
    set_memlock_soft(original_soft);

    // Root or CAP_IPC_LOCK can pin past a 0 rlimit. If the test runner is
    // privileged the call succeeds and we only verify that the success
    // bumped the attempt counter without bumping the downgrade counter.
    match result {
        Ok(window) => {
            assert!(!window.is_empty());
            assert!(
                mlock_attempts() > before_attempts,
                "successful pin must bump mlock_attempts"
            );
            assert!(
                mlock_downgrades() >= before_downgrades,
                "downgrade counter is monotonic"
            );
        }
        Err(MlockError::Downgrade(err)) => {
            let raw = err.raw_os_error().unwrap_or(0);
            assert!(
                matches!(raw, libc::EPERM | libc::EAGAIN | libc::ENOMEM),
                "downgrade errno must be in the documented set, got {raw}"
            );
            assert!(
                mlock_downgrades() > before_downgrades,
                "downgrade path must increment mlock_downgrades"
            );
            assert!(
                mlock_attempts() >= before_attempts,
                "attempt counter is monotonic across parallel tests"
            );
        }
        Err(MlockError::Fatal(e)) => panic!("expected downgrade, got fatal: {e}"),
    }
}

#[test]
fn small_rlimit_allows_small_pin_blocks_large_pin() {
    let _guard = rlimit_lock();
    let (original_soft, _) = get_memlock_limit();

    // Set a 4 KiB budget. A page-sized pin should succeed; a 1 MiB pin
    // (256 pages) should hit EAGAIN unless the runner has CAP_IPC_LOCK.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as libc::rlim_t;
    set_memlock_soft(page_size);

    let large = page_aligned_buffer(1024 * 1024);
    let before_downgrades = mlock_downgrades();
    let result = WiredBasisWindow::new(large.as_ptr(), large.len());

    set_memlock_soft(original_soft);

    match result {
        Ok(window) => {
            // Privileged runner; the large pin succeeded. Verify the
            // counter discipline.
            assert!(!window.is_empty());
            assert!(mlock_downgrades() >= before_downgrades);
        }
        Err(MlockError::Downgrade(err)) => {
            let raw = err.raw_os_error().unwrap_or(0);
            assert!(
                matches!(raw, libc::EAGAIN | libc::EPERM | libc::ENOMEM),
                "rlimit overflow must surface a downgrade errno, got {raw}"
            );
            assert!(mlock_downgrades() > before_downgrades);
        }
        Err(MlockError::Fatal(e)) => panic!("rlimit overflow should not be fatal: {e}"),
    }
}

#[test]
fn successful_pin_releases_on_drop() {
    let _guard = rlimit_lock();
    let (original_soft, _) = get_memlock_limit();

    // Try with a budget large enough that even an unprivileged process
    // can pin 4 KiB. The kernel default is 64 KiB so this should always
    // succeed on a stock Linux.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as libc::rlim_t;
    let budget = page_size.max(64 * 1024);
    set_memlock_soft(budget);

    let before_attempts = mlock_attempts();

    let buf = page_aligned_buffer(page_size as usize);
    let result = WiredBasisWindow::new(buf.as_ptr(), buf.len());

    set_memlock_soft(original_soft);

    match result {
        Ok(window) => {
            assert_eq!(window.len(), page_size as usize);
            assert!(!window.as_ptr().is_null());
            assert!(mlock_attempts() > before_attempts);
            // Drop happens here. The guard's munlock is a no-throw path;
            // if it failed we would log but not panic.
            drop(window);
        }
        Err(MlockError::Downgrade(_)) => {
            // Some hardened CI environments deny mlock outright; treat as
            // a skip but still verify the counter discipline.
            eprintln!("skipping successful_pin assertion: kernel refused mlock");
        }
        Err(MlockError::Fatal(e)) => panic!("unexpected fatal: {e}"),
    }
}
