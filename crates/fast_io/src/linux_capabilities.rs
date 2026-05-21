//! Cached runtime probes for Linux-specific kernel capabilities used by the
//! SEC-1 dirfd sandbox.
//!
//! The SEC-1 family of patches replaces path-based `*` syscalls
//! (`open`, `rename`, `link`, `unlink`, ...) with their dirfd-anchored `*at`
//! siblings, preferring the `*2` variants (`openat2`, `renameat2`, ...) when
//! the kernel supports them. Each call site needs to know whether the
//! strict-resolution `openat2(2)` path is available so it can pick the right
//! variant; rather than each site reinventing the probe, this module exposes
//! [`openat2_supported`] as the single source of truth.
//!
//! The module is gated `#[cfg(unix)]` so callers can use the helper without
//! `#[cfg(target_os = "linux")]` branching - on non-Linux Unix targets the
//! probe is a compile-time constant `false`. Windows callers do not use
//! this path at all (see the SEC-1.l audit on NTFS handle-based APIs).

/// Returns `true` when the running kernel supports `openat2(2)` and `false`
/// otherwise.
///
/// On Linux the first invocation issues a no-op
/// `openat2(AT_FDCWD, ".", { resolve: 0 })` syscall and caches the outcome
/// in a [`OnceLock`](std::sync::OnceLock):
///
/// - A non-negative return value (a fresh fd which we close immediately)
///   means the kernel supports the syscall - cache `true`.
/// - `ENOSYS` means the kernel pre-dates Linux 5.6 (or the syscall is
///   blocked by seccomp) - cache `false`.
/// - Any other errno still means the syscall reached the kernel, so the
///   ABI is supported - cache `true`. Callers will see the real errno on
///   their own subsequent invocations.
///
/// Subsequent invocations on Linux read the cached value and skip the
/// syscall. The probe is thread-safe; the first caller wins the
/// `OnceLock::set`, and every later caller observes the same result.
///
/// On non-Linux Unix targets this is a compile-time `false`.
#[must_use]
pub fn openat2_supported() -> bool {
    imp::openat2_supported()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::OnceLock;

    static OPENAT2_AVAILABLE: OnceLock<bool> = OnceLock::new();

    pub(super) fn openat2_supported() -> bool {
        if let Some(cached) = OPENAT2_AVAILABLE.get().copied() {
            return cached;
        }
        let result = probe();
        let _ = OPENAT2_AVAILABLE.set(result);
        // Re-read in case another thread won the race; both writers wrote
        // the same value, so the observed result is stable.
        OPENAT2_AVAILABLE.get().copied().unwrap_or(result)
    }

    fn probe() -> bool {
        // No-op probe path: "." resolves the current working directory,
        // which is guaranteed to exist for any running process. The kernel
        // returns a fresh fd we close immediately, or `ENOSYS` if the
        // syscall is unavailable.
        let path = c".";

        // SAFETY: a single unsafe scope wraps every FFI touch the probe
        // performs:
        //
        // 1. `std::mem::zeroed::<open_how>()` - `libc::open_how` is
        //    `#[non_exhaustive]`, so a struct expression is unavailable. The
        //    type is repr(C) with integer-only fields, and an all-zero bit
        //    pattern is a valid value (the documented "no constraint" mode
        //    for every `openat2(2)` knob).
        //
        // 2. `libc::syscall(SYS_openat2, AT_FDCWD, path, &how, size)` -
        //    `path` is a valid NUL-terminated C string with static
        //    lifetime; `how` is a fully-initialised `open_how` whose
        //    address and `size_of::<open_how>()` we hand to the kernel as
        //    required by the syscall ABI. The kernel does not retain the
        //    pointer past return. A non-negative return value is a fresh,
        //    owned fd with `O_CLOEXEC` set.
        //
        // 3. `libc::close(raw)` - `raw` is the fd we just received and
        //    have not aliased or leaked. Closing it here keeps the probe
        //    descriptor-neutral. We deliberately ignore the close return
        //    value: the probe is a read-only directory open, so close
        //    cannot return EIO or EINTR meaningfully.
        #[allow(unsafe_code)]
        unsafe {
            let mut how: libc::open_how = std::mem::zeroed();
            how.flags = (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
            how.mode = 0;
            how.resolve = 0;

            let raw = libc::syscall(
                libc::SYS_openat2,
                libc::AT_FDCWD,
                path.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            );

            if raw >= 0 {
                libc::close(raw as libc::c_int);
                return true;
            }
        }

        let errno = std::io::Error::last_os_error().raw_os_error();
        errno != Some(libc::ENOSYS)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub(super) fn openat2_supported() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openat2_supported_returns_bool_without_panic() {
        // Two back-to-back calls exercise both the "no cache yet" path and
        // the "cache hit" path within a single test invocation. Either
        // outcome (true on Linux 5.6+, false elsewhere) is valid; what we
        // assert is the absence of panics and a consistent return value
        // across the two calls.
        let first = openat2_supported();
        let second = openat2_supported();
        assert_eq!(
            first, second,
            "openat2_supported() must return a stable value across calls"
        );
    }

    #[test]
    fn openat2_supported_is_cached() {
        // Hammer the probe from many calls and confirm the cached result
        // never wavers. The probe must not depend on transient kernel
        // state, environment, or call ordering.
        let baseline = openat2_supported();
        for _ in 0..1024 {
            assert_eq!(
                openat2_supported(),
                baseline,
                "openat2_supported() result must be cached and stable"
            );
        }
    }

    #[test]
    fn openat2_supported_is_thread_safe() {
        // Spawn several threads that all race to read the probe. The
        // OnceLock-backed cache must serialise the first writer and let
        // every other thread observe the same outcome.
        let baseline = openat2_supported();
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(openat2_supported))
            .collect();
        for handle in handles {
            let observed = handle.join().expect("probe thread panicked");
            assert_eq!(
                observed, baseline,
                "openat2_supported() must agree across threads"
            );
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn openat2_supported_is_false_on_non_linux() {
        // Non-Linux Unix targets (macOS, *BSD, illumos) do not implement
        // `openat2(2)`. The helper must short-circuit to `false` without
        // any syscall traffic.
        assert!(!openat2_supported());
    }
}
