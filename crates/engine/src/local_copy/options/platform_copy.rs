//! Platform copy strategy injection for local copy options.
//!
//! Exposes a setter and accessor for the `platform_copy` field on
//! [`LocalCopyOptions`]. The strategy is consulted by whole-file copy
//! paths (clonefile/CopyFileExW/std::fs::copy fallbacks) so callers and
//! tests can substitute a custom implementation.

use std::sync::Arc;

use fast_io::PlatformCopy;

use super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Replaces the platform copy strategy used by whole-file fast paths.
    ///
    /// Defaults to [`fast_io::DefaultPlatformCopy`]; tests can inject a fake
    /// implementation to verify dispatch.
    #[must_use]
    pub fn with_platform_copy(mut self, platform_copy: Arc<dyn PlatformCopy>) -> Self {
        self.platform_copy = platform_copy;
        self
    }

    /// Returns the configured platform copy strategy.
    #[must_use]
    pub fn platform_copy(&self) -> &Arc<dyn PlatformCopy> {
        &self.platform_copy
    }

    /// Sets the I/O-level zero-copy policy for the file content path,
    /// mirroring `--zero-copy` / `--no-zero-copy`.
    #[must_use]
    pub const fn with_zero_copy_policy(mut self, policy: fast_io::ZeroCopyPolicy) -> Self {
        self.zero_copy = policy;
        self
    }

    /// Returns the configured I/O-level zero-copy policy.
    #[must_use]
    pub const fn zero_copy_policy(&self) -> fast_io::ZeroCopyPolicy {
        self.zero_copy
    }

    /// Reports whether the content path may hand a copy to the kernel
    /// (io_uring, then `copy_file_range`) instead of reading it in userspace.
    ///
    /// `Auto` and `Enabled` both allow it; only
    /// [`fast_io::ZeroCopyPolicy::Disabled`] forces the portable read/write
    /// loop. That is the contract `ZeroCopyPolicy` already documents - "the
    /// chain is bypassed" - and the content path is the second half of it: the
    /// first half swaps the clone/reflink tier via [`Self::with_platform_copy`],
    /// which does not reach the kernel-side movers that run when no clone
    /// applied.
    #[must_use]
    pub const fn kernel_content_copy_allowed(&self) -> bool {
        !matches!(self.zero_copy, fast_io::ZeroCopyPolicy::Disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fast_io::{CopyMethod, CopyResult, DefaultPlatformCopy, ZeroCopyPolicy};
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct CountingPlatformCopy {
        calls: AtomicUsize,
    }

    impl PlatformCopy for CountingPlatformCopy {
        fn copy_file(&self, _src: &Path, _dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
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
    fn default_platform_copy_is_set() {
        let opts = LocalCopyOptions::new();
        assert_eq!(
            opts.platform_copy().preferred_method(0),
            DefaultPlatformCopy::new().preferred_method(0)
        );
    }

    #[test]
    fn with_platform_copy_overrides_default() {
        let counting = Arc::new(CountingPlatformCopy::default());
        let opts = LocalCopyOptions::new().with_platform_copy(counting.clone());
        // Invoke through the option to confirm the injected impl is reachable.
        let result = opts
            .platform_copy()
            .copy_file(Path::new("/dev/null"), Path::new("/dev/null"), 0)
            .expect("counting strategy returns Ok");
        assert_eq!(result.method, CopyMethod::StandardCopy);
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
    }

    /// The content path may hand a copy to the kernel unless the operator
    /// disabled it, so the default must not change today's behaviour.
    #[test]
    fn zero_copy_policy_defaults_to_auto_and_allows_the_kernel_content_copy() {
        let opts = LocalCopyOptions::new();
        assert_eq!(opts.zero_copy_policy(), ZeroCopyPolicy::Auto);
        assert!(opts.kernel_content_copy_allowed());
    }

    /// Exhaustive over the three arms: only `Disabled` forces the portable
    /// read/write loop. Asserting the whole enum rather than the one arm the
    /// fix targets is what stops a later variant from silently defaulting to
    /// "kernel copy allowed" - the direction that would make `--no-zero-copy`
    /// half-inert again.
    #[test]
    fn only_disabled_forbids_the_kernel_content_copy() {
        for (policy, allowed) in [
            (ZeroCopyPolicy::Auto, true),
            (ZeroCopyPolicy::Enabled, true),
            (ZeroCopyPolicy::Disabled, false),
        ] {
            let opts = LocalCopyOptions::new().with_zero_copy_policy(policy);
            assert_eq!(opts.zero_copy_policy(), policy);
            assert_eq!(
                opts.kernel_content_copy_allowed(),
                allowed,
                "{policy:?} must {} the kernel content copy",
                if allowed { "allow" } else { "forbid" }
            );
        }
    }

    /// `--no-zero-copy` governs two independent mechanisms. This pins that
    /// setting the content policy leaves the clone/reflink tier alone, so a
    /// future edit cannot collapse them into one field and quietly drop half
    /// the flag's contract.
    #[test]
    fn the_content_policy_is_independent_of_the_platform_copy_tier() {
        let counting = Arc::new(CountingPlatformCopy::default());
        let opts = LocalCopyOptions::new()
            .with_platform_copy(counting.clone())
            .with_zero_copy_policy(ZeroCopyPolicy::Disabled);

        assert!(!opts.kernel_content_copy_allowed());
        opts.platform_copy()
            .copy_file(Path::new("/dev/null"), Path::new("/dev/null"), 0)
            .expect("the injected tier is still the configured one");
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
    }
}
