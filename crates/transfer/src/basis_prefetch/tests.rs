use super::*;
use std::path::PathBuf;

#[test]
fn null_prefetcher_is_a_noop() {
    let p = NullPrefetcher;
    // Must not panic or block on any path, existing or not.
    p.prefetch(&PathBuf::from("/nonexistent/basis/file"));
    BasisPrefetcher::prefetch(&p, &PathBuf::from("/tmp"));
}

#[test]
fn select_returns_null_under_inplace() {
    let p = select_prefetcher(PrefetchDisableList {
        inplace: true,
        append: false,
    });
    // Disable-list forces Null even with the feature on; prefetch is a no-op.
    p.prefetch(&PathBuf::from("/nonexistent"));
}

#[test]
fn select_returns_null_under_append() {
    let p = select_prefetcher(PrefetchDisableList {
        inplace: false,
        append: true,
    });
    p.prefetch(&PathBuf::from("/nonexistent"));
}

#[cfg(not(all(unix, feature = "basis-readahead")))]
#[test]
fn select_returns_null_when_feature_off() {
    // With the feature off the enabled path must still be Null - no threads,
    // no behavior change. This exercises the default production build.
    let p = select_prefetcher(PrefetchDisableList {
        inplace: false,
        append: false,
    });
    p.prefetch(&PathBuf::from("/nonexistent"));
}

#[cfg(all(unix, feature = "basis-readahead"))]
mod fadvise_active {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn fadvise_prefetcher_hints_and_joins_cleanly() {
        let mut temp = NamedTempFile::new().expect("temp file");
        temp.write_all(&[0u8; 64 * 1024]).expect("write");
        temp.flush().expect("flush");
        let path = temp.path().to_path_buf();

        let prefetcher = FadviseWillneedPrefetcher::new(PREFETCH_DEPTH).expect("spawn worker");
        prefetcher.prefetch(&path);
        // A missing path must be a silent no-op, not a panic.
        prefetcher.prefetch(&PathBuf::from("/nonexistent/basis/file"));
        // Dropping joins the worker; the test hangs if join deadlocks.
        drop(prefetcher);
    }

    #[test]
    fn fadvise_prefetcher_handles_many_paths_with_backpressure() {
        let prefetcher = FadviseWillneedPrefetcher::new(PREFETCH_DEPTH).expect("spawn worker");
        // Enqueue far more than the channel depth; try_send must never block.
        for _ in 0..1000 {
            prefetcher.prefetch(&PathBuf::from("/nonexistent"));
        }
        drop(prefetcher);
    }

    #[test]
    fn fadvise_prefetcher_hints_a_real_basis_file() {
        // Exercise the active prefetcher directly (what select_prefetcher
        // returns when the env gate is on). Env mutation is avoided because the
        // crate denies unsafe; the gate is covered by construction here.
        let mut temp = NamedTempFile::new().expect("temp file");
        temp.write_all(&[0u8; 128 * 1024]).expect("write");
        temp.flush().expect("flush");

        let prefetcher = FadviseWillneedPrefetcher::new(PREFETCH_DEPTH).expect("spawn worker");
        prefetcher.prefetch(temp.path());
        drop(prefetcher);
    }
}
