//! Total physical memory detection.
//!
//! Used by the buffer pool's opt-in RAM-derived memory cap
//! (`OC_RSYNC_BUFFER_POOL_MEMORY_CAP=auto`) to size the outstanding-memory
//! backpressure ceiling as a fraction of installed RAM. Detection is
//! best-effort: callers treat `None` as "unknown" and leave the pool uncapped
//! rather than guessing, so a platform without a query path degrades to the
//! historical (uncapped) default instead of failing.

/// Returns the machine's total physical RAM in bytes, or `None` when it cannot
/// be determined on this platform.
///
/// On Unix this multiplies `sysconf(_SC_PHYS_PAGES)` by the page size; on
/// Windows it reads `MEMORYSTATUSEX::ullTotalPhys`. Any query failure yields
/// `None` so callers degrade gracefully instead of panicking.
#[must_use]
pub fn total_physical_memory() -> Option<u64> {
    query()
}

#[cfg(unix)]
fn query() -> Option<u64> {
    // SAFETY: `sysconf` takes a single integer name and has no preconditions;
    // it returns -1 (which we reject below) for an unsupported name. Both
    // `_SC_PHYS_PAGES` and `_SC_PAGESIZE` are read-only queries with no
    // out-parameters, so the call cannot corrupt memory.
    #[allow(unsafe_code)]
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    // SAFETY: same contract as the `_SC_PHYS_PAGES` query above.
    #[allow(unsafe_code)]
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if pages > 0 && page_size > 0 {
        (pages as u64).checked_mul(page_size as u64)
    } else {
        None
    }
}

#[cfg(windows)]
fn query() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    // SAFETY: `MEMORYSTATUSEX` is a plain-old-data struct that is valid when
    // zeroed except for `dwLength`, which the API requires be set to the
    // struct size before the call; we set it immediately below.
    #[allow(unsafe_code)]
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?;
    // SAFETY: `status` is a valid, initialized out-parameter for the duration
    // of the call; `GlobalMemoryStatusEx` populates every field it writes and
    // returns non-zero on success.
    #[allow(unsafe_code)]
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok != 0 {
        Some(status.ullTotalPhys)
    } else {
        None
    }
}

#[cfg(not(any(unix, windows)))]
fn query() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    #[test]
    fn reports_a_plausible_amount_on_supported_platforms() {
        // On every CI runner the query path is available and the machine has
        // far more than 16 MiB of RAM, so a `Some` well above that lower bound
        // proves both the syscall wiring and the pages * page_size arithmetic.
        let total = total_physical_memory().expect("physical memory is queryable");
        assert!(
            total > 16 * 1024 * 1024,
            "implausibly small physical memory reported: {total} bytes"
        );
    }
}
