//! Process heap statistics for the `--info=stats3` diagnostic block.
//!
//! upstream: `main.c:484` `show_malloc_stats()`, called from `handle_stats()`
//! (`main.c:337-340`) under `INFO_GTE(STATS, 3)`. Upstream reads glibc's
//! `mallinfo2()` behind `#ifdef MEM_ALLOC_INFO` (`rsync.h:1543`), so the block
//! is absent on platforms whose allocator cannot report.
//!
//! oc installs jemalloc as its global allocator on unix
//! (`src/bin/oc-rsync.rs`), so glibc's arena is unused and `mallinfo2` would
//! describe a heap oc never allocates from. The equivalent counters come from
//! jemalloc's `mallctl` `stats.*` namespace instead, which is why the rendered
//! field names differ from upstream's - see `HeapStats`.
//!
//! This module lives in `fast_io` because it is the crate permitted to hold
//! platform FFI and expose a safe API over it.

/// A single sample of the active allocator's heap counters.
///
/// The field set is jemalloc's, not glibc's: there is no faithful mapping
/// between `mallinfo2`'s arena/mmap split and jemalloc's extent accounting, so
/// oc reports what its allocator actually measures rather than inventing
/// upstream-shaped numbers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeapStats {
    /// Bytes in live allocations.
    pub allocated: u64,
    /// Bytes in pages backing live allocations.
    pub active: u64,
    /// Bytes the allocator holds for its own bookkeeping.
    pub metadata: u64,
    /// Bytes in physically resident pages.
    pub resident: u64,
    /// Bytes in mapped extents.
    pub mapped: u64,
    /// Bytes retained by the allocator rather than returned to the OS.
    pub retained: u64,
}

/// Samples the active allocator, or `None` where it cannot report.
///
/// `None` is upstream's `#ifdef MEM_ALLOC_INFO`-absent case: the caller renders
/// no block at all rather than a block of zeroes.
#[must_use]
pub fn heap_stats() -> Option<HeapStats> {
    imp::heap_stats()
}

#[cfg(unix)]
mod imp {
    use super::HeapStats;
    use std::ffi::c_void;
    use std::mem::size_of;

    // jemalloc's `mallctl` names are NUL-terminated C strings.
    const EPOCH: &[u8] = b"epoch\0";
    const ALLOCATED: &[u8] = b"stats.allocated\0";
    const ACTIVE: &[u8] = b"stats.active\0";
    const METADATA: &[u8] = b"stats.metadata\0";
    const RESIDENT: &[u8] = b"stats.resident\0";
    const MAPPED: &[u8] = b"stats.mapped\0";
    const RETAINED: &[u8] = b"stats.retained\0";

    pub(super) fn heap_stats() -> Option<HeapStats> {
        // The `stats.*` counters are refreshed only when the epoch advances, so
        // a read without this reports the values from allocator init.
        advance_epoch()?;
        Some(HeapStats {
            allocated: read_counter(ALLOCATED)?,
            active: read_counter(ACTIVE)?,
            metadata: read_counter(METADATA)?,
            resident: read_counter(RESIDENT)?,
            mapped: read_counter(MAPPED)?,
            retained: read_counter(RETAINED)?,
        })
    }

    /// Writes the `epoch` control, which makes jemalloc re-cache `stats.*`.
    #[allow(unsafe_code)]
    fn advance_epoch() -> Option<()> {
        let mut old: u64 = 0;
        let mut old_len = size_of::<u64>();
        let mut new: u64 = 1;
        // SAFETY: `EPOCH` is a NUL-terminated name. `old`/`new` are live,
        // exclusively borrowed `u64`s, and jemalloc documents `epoch` as a
        // `uint64_t` control, so both lengths describe exactly the storage
        // being handed over. `mallctl` writes at most `old_len` bytes to `oldp`
        // and reads exactly `newlen` from `newp`.
        let rc = unsafe {
            tikv_jemalloc_sys::mallctl(
                EPOCH.as_ptr().cast(),
                std::ptr::from_mut(&mut old).cast::<c_void>(),
                &raw mut old_len,
                std::ptr::from_mut(&mut new).cast::<c_void>(),
                size_of::<u64>(),
            )
        };
        (rc == 0).then_some(())
    }

    /// Reads one `size_t`-valued `stats.*` counter.
    #[allow(unsafe_code)]
    fn read_counter(name: &[u8]) -> Option<u64> {
        let mut value: usize = 0;
        let mut len = size_of::<usize>();
        // SAFETY: every `name` here is a NUL-terminated `stats.*` control that
        // jemalloc documents as `size_t`, matching `value`'s type and `len`.
        // `newp` is null with `newlen` 0, which is `mallctl`'s read-only form,
        // so the call cannot write through a dangling pointer.
        let rc = unsafe {
            tikv_jemalloc_sys::mallctl(
                name.as_ptr().cast(),
                std::ptr::from_mut(&mut value).cast::<c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && len == size_of::<usize>()).then_some(value as u64)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::HeapStats;

    pub(super) fn heap_stats() -> Option<HeapStats> {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::heap_stats;

    // Mirrors the binary's allocator selection so the counters under test are
    // the ones this process actually allocates from.
    #[global_allocator]
    static TEST_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

    #[test]
    fn a_sample_reports_live_allocations() {
        let stats = heap_stats().expect("jemalloc reports stats on unix");
        assert!(
            stats.allocated > 0,
            "a running process always holds live allocations: {stats:?}"
        );
        assert!(
            stats.resident >= stats.allocated,
            "resident pages must cover live allocations: {stats:?}"
        );
    }

    #[test]
    fn allocating_between_samples_moves_the_counter() {
        let before = heap_stats().expect("jemalloc reports stats on unix");
        let ballast: Vec<u8> = vec![0u8; 8 * 1024 * 1024];
        let during = heap_stats().expect("jemalloc reports stats on unix");
        assert!(
            during.allocated > before.allocated,
            "the epoch advance must expose the new allocation: \
             {} -> {}",
            before.allocated,
            during.allocated
        );
        drop(ballast);
    }
}
