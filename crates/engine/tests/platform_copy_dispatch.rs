//! Integration tests verifying `LocalCopyOptions` plumbs a custom
//! [`PlatformCopy`] strategy all the way through the local-copy executor.
//!
//! The macOS whole-file fast path in
//! `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs`
//! dispatches new whole-file copies through `options.platform_copy()`.
//! These tests inject a counting fake to confirm the executor reaches the
//! injected trait object rather than calling the platform primitives
//! directly. The fake reports a non-zero-copy result so the executor falls
//! through to the standard copy path, leaving the destination file in
//! place; this preserves current behaviour exactly while still proving
//! that the dispatch hook is wired.
//!
//! Other platforms do not yet expose an executor-level whole-file fast
//! path that bypasses the trait, so the dispatch assertion is gated on
//! macOS. The plumbing assertion (the option holds and exposes the
//! injected strategy) runs on every platform.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use engine::local_copy::{
    LocalCopyExecution, LocalCopyOptions, LocalCopyOptionsBuilder, LocalCopyPlan,
};
use fast_io::{CopyMethod, CopyResult, PlatformCopy};
use tempfile::tempdir;

/// `PlatformCopy` fake that counts invocations and reports a non-zero-copy
/// result so the executor falls through to its standard copy path.
#[derive(Debug, Default)]
struct CountingPlatformCopy {
    calls: AtomicUsize,
}

impl CountingPlatformCopy {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PlatformCopy for CountingPlatformCopy {
    fn copy_file(&self, _src: &Path, _dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Returning `StandardCopy` (not zero-copy) signals the executor to
        // discard this attempt and fall through to its portable copy path.
        Ok(CopyResult::new(0, CopyMethod::StandardCopy))
    }

    fn supports_reflink(&self) -> bool {
        false
    }

    fn preferred_method(&self, _size: u64) -> CopyMethod {
        CopyMethod::StandardCopy
    }
}

#[test]
fn options_holds_injected_platform_copy() {
    let counting: Arc<CountingPlatformCopy> = Arc::new(CountingPlatformCopy::default());
    let options = LocalCopyOptions::new().with_platform_copy(counting.clone());

    // Calling through the option's accessor reaches the injected impl.
    let result = options
        .platform_copy()
        .copy_file(Path::new("/dev/null"), Path::new("/dev/null"), 0)
        .expect("counting strategy returns Ok");

    assert_eq!(result.method, CopyMethod::StandardCopy);
    assert_eq!(counting.calls(), 1);
}

#[test]
fn builder_propagates_injected_platform_copy() {
    let counting: Arc<CountingPlatformCopy> = Arc::new(CountingPlatformCopy::default());
    let options = LocalCopyOptionsBuilder::new()
        .platform_copy(counting.clone())
        .build()
        .expect("builder produces valid options");

    options
        .platform_copy()
        .copy_file(Path::new("/dev/null"), Path::new("/dev/null"), 0)
        .expect("counting strategy returns Ok");

    assert_eq!(counting.calls(), 1);
}

/// On macOS the executor dispatches new whole-file copies through
/// `options.platform_copy()`. This test runs a single-file copy via
/// `LocalCopyPlan` and asserts the injected fake is invoked at least once.
#[cfg(target_os = "macos")]
#[test]
fn executor_dispatches_whole_file_copy_through_platform_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src.txt");
    let destination = temp.path().join("dst.txt");
    fs::write(&source, b"oc-rsync platform copy dispatch test").expect("write source");

    let counting: Arc<CountingPlatformCopy> = Arc::new(CountingPlatformCopy::default());

    let operands = vec![
        OsString::from(source.as_os_str()),
        OsString::from(destination.as_os_str()),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
    let options = LocalCopyOptions::new().with_platform_copy(counting.clone());

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        counting.calls() >= 1,
        "executor should dispatch whole-file copy through the injected PlatformCopy \
         (observed {} invocations)",
        counting.calls()
    );

    // The fake reports a non-zero-copy result, so the executor falls through
    // to the standard copy path. The destination must still exist with the
    // correct content - confirming the fall-through preserves behaviour.
    assert!(destination.exists(), "destination file should exist");
    let copied = fs::read(&destination).expect("read destination");
    assert_eq!(copied, b"oc-rsync platform copy dispatch test");
}
