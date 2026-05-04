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
}

#[cfg(test)]
mod tests {
    use super::*;
    use fast_io::{CopyMethod, CopyResult, DefaultPlatformCopy};
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
        // The default strategy is callable.
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
}
