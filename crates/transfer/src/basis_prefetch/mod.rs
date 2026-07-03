//! Basis-file read-ahead prefetch (default-off, feature-gated).
//!
//! The decoupled receiver pipeline resolves each upcoming file's basis path
//! ahead of the disk-commit stage, then later blocks the main thread on a
//! serial `MapFile::open` of that basis during delta-token processing. On an
//! I/O-wait-bound receiver those blocking basis reads dominate wall-clock time
//! while the disk sits underused.
//!
//! A [`BasisPrefetcher`] issues a kernel read-ahead hint
//! (`posix_fadvise(POSIX_FADV_WILLNEED)`) on look-ahead basis paths so their
//! pages are warm in the page cache by the time the pipeline reaches them.
//!
//! # Correctness
//!
//! This is a pure timing optimization. `posix_fadvise(WILLNEED)` never changes
//! the bytes read from the basis file, so the wire output and the reconstructed
//! destination are byte-identical whether or not a prefetcher runs. The only
//! observable effect is that later basis reads may complete faster.
//!
//! The default [`NullPrefetcher`] does nothing and spawns no threads. The
//! active [`FadviseWillneedPrefetcher`] is only compiled in under the
//! `basis-readahead` feature on Unix, and even then is only selected when the
//! transfer does not mutate a file's basis in place (see [`select_prefetcher`]).

use std::path::Path;
use std::sync::Arc;

#[cfg(all(unix, feature = "basis-readahead"))]
mod fadvise;

#[cfg(all(unix, feature = "basis-readahead"))]
pub use fadvise::FadviseWillneedPrefetcher;

/// Number of look-ahead files to prefetch and the prefetch channel depth.
///
/// Matches the pipeline's short look-ahead window: prefetching further than
/// this wastes page cache without reducing main-thread stalls, and the bounded
/// channel of the same depth provides natural backpressure.
pub const PREFETCH_DEPTH: usize = 4;

/// Issues read-ahead hints for upcoming basis files.
///
/// Implementations must be cheap to call and must never block the caller for
/// a meaningful duration - the hint is fire-and-forget. A failed or ignored
/// hint is a silent no-op.
pub trait BasisPrefetcher: Send + Sync {
    /// Requests that the kernel begin warming the page cache for `path`.
    ///
    /// Best-effort: any error (missing file, open failure, unsupported
    /// platform) is silently ignored.
    fn prefetch(&self, path: &Path);
}

/// Prefetcher that does nothing. The default when the feature is off or the
/// transfer is on the correctness disable-list.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullPrefetcher;

impl BasisPrefetcher for NullPrefetcher {
    #[inline]
    fn prefetch(&self, _path: &Path) {}
}

/// Correctness disable-list inputs for [`select_prefetcher`].
///
/// Prefetch must be disabled whenever writing one file can mutate the basis of
/// a later file, because warming a stale basis into cache would not help and
/// the in-place write races the read-ahead. `--inplace` and `--append` both
/// write directly into the destination that may serve as a later basis.
#[derive(Debug, Clone, Copy)]
pub struct PrefetchDisableList {
    /// `--inplace`: destination is written in place, mutating potential bases.
    pub inplace: bool,
    /// `--append`: destination is extended in place, mutating potential bases.
    pub append: bool,
}

/// Selects the prefetcher for a transfer.
///
/// Returns [`NullPrefetcher`] when the `basis-readahead` feature is not
/// compiled in, when the platform is not Unix, when the runtime env activation
/// (`OC_RSYNC_BASIS_READAHEAD=1`) is absent, or when the transfer is on the
/// correctness disable-list. Otherwise returns an active
/// [`FadviseWillneedPrefetcher`].
///
/// The default build always returns [`NullPrefetcher`], so production behavior
/// is unchanged unless the feature is compiled in AND enabled.
#[must_use]
pub fn select_prefetcher(disable: PrefetchDisableList) -> Arc<dyn BasisPrefetcher> {
    #[cfg(all(unix, feature = "basis-readahead"))]
    {
        if !disable.inplace && !disable.append && basis_readahead_env_enabled() {
            if let Ok(p) = FadviseWillneedPrefetcher::new(PREFETCH_DEPTH) {
                return Arc::new(p);
            }
        }
        Arc::new(NullPrefetcher)
    }

    #[cfg(not(all(unix, feature = "basis-readahead")))]
    {
        let _ = disable;
        Arc::new(NullPrefetcher)
    }
}

/// Runtime activation gate, consulted only when the feature is compiled in.
///
/// Lets a single feature-enabled bench binary toggle prefetch via
/// `OC_RSYNC_BASIS_READAHEAD=1` without a CLI surface. Absent or any other
/// value keeps prefetch off.
#[cfg(all(unix, feature = "basis-readahead"))]
fn basis_readahead_env_enabled() -> bool {
    std::env::var_os("OC_RSYNC_BASIS_READAHEAD").is_some_and(|v| v == "1")
}

#[cfg(test)]
mod tests;
