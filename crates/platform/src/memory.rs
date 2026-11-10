//! Process-wide memory policy applied at startup.
//!
//! oc-rsync allocates through jemalloc, whose extents are 2 MiB aligned. On a
//! host with transparent huge pages set to `always`, the kernel backs each
//! extent with a whole huge page after a single small allocation touches it.
//! Measured on a 100k-file pull, that adds about 40 MiB of resident memory
//! (59.6-64.6 MiB peak against 18.8-19.6 MiB with THP disabled) that the
//! transfer never uses. The step comes and goes with huge-page availability,
//! so peak RSS swings between the two levels from one run to the next.
//! Upstream rsync's small glibc heap stays near 11 MiB, so this is an oc-only
//! policy with no upstream counterpart.

use std::io;

/// Opts this process out of transparent huge pages.
///
/// Sets `PR_SET_THP_DISABLE`, which applies to the whole address space and is
/// inherited by children and preserved across `execve`. Call it once at
/// startup, before allocation-heavy work, so no extent is promoted first.
///
/// # Errors
///
/// Returns the `prctl` error, `EINVAL` on kernels older than 3.15, which lack
/// the flag. The caller treats it as best-effort.
#[cfg(target_os = "linux")]
pub fn disable_transparent_huge_pages() -> io::Result<()> {
    nix::sys::prctl::set_thp_disable(true).map_err(io::Error::from)
}

/// No-op where transparent huge pages cannot be disabled per process.
///
/// # Errors
///
/// Never fails.
#[cfg(not(target_os = "linux"))]
pub fn disable_transparent_huge_pages() -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::disable_transparent_huge_pages;

    /// The `THP_enabled` field of `/proc/self/status`: 1 unless this process
    /// opted out with `PR_SET_THP_DISABLE` (Linux 5.0 and later).
    fn thp_enabled() -> String {
        let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
        status
            .lines()
            .find_map(|line| line.strip_prefix("THP_enabled:"))
            .map(|value| value.trim().to_owned())
            .expect("kernel reports THP_enabled")
    }

    /// The opt-out reaches the kernel: the flag reads 1 before and 0 after.
    ///
    /// The starting value is asserted so the cell cannot pass vacuously in an
    /// environment that already disabled THP for the test process. nextest
    /// runs each test in its own process, so the flag does not leak.
    #[test]
    fn opt_out_clears_thp_enabled() {
        assert_eq!(
            thp_enabled(),
            "1",
            "environment already disabled THP for this process"
        );

        disable_transparent_huge_pages().expect("PR_SET_THP_DISABLE");

        assert_eq!(
            thp_enabled(),
            "0",
            "PR_SET_THP_DISABLE must clear THP_enabled"
        );
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::disable_transparent_huge_pages;

    /// The stub succeeds, so the best-effort call site has nothing to handle.
    #[test]
    fn stub_succeeds() {
        assert!(disable_transparent_huge_pages().is_ok());
    }
}
