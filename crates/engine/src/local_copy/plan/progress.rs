use std::path::Path;
use std::time::Duration;

/// Snapshot describing in-flight progress for a transfer action.
#[derive(Clone, Copy, Debug)]
pub struct LocalCopyProgress<'a> {
    relative_path: &'a Path,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
}

impl<'a> LocalCopyProgress<'a> {
    /// Creates a new [`LocalCopyProgress`] snapshot.
    #[must_use]
    pub const fn new(
        relative_path: &'a Path,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
    ) -> Self {
        Self {
            relative_path,
            bytes_transferred,
            total_bytes,
            elapsed,
        }
    }

    /// Returns the path associated with the progress snapshot.
    #[must_use]
    pub const fn relative_path(&self) -> &'a Path {
        self.relative_path
    }

    /// Returns the number of bytes transferred so far.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this action, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent on this action.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn new_creates_progress() {
        let path = Path::new("test.txt");
        let progress = LocalCopyProgress::new(path, 100, Some(200), Duration::from_secs(1));
        assert_eq!(progress.relative_path(), path);
        assert_eq!(progress.bytes_transferred(), 100);
        assert_eq!(progress.total_bytes(), Some(200));
        assert_eq!(progress.elapsed(), Duration::from_secs(1));
    }

    #[test]
    fn relative_path_returns_correct_path() {
        let path = Path::new("subdir/file.txt");
        let progress = LocalCopyProgress::new(path, 0, None, Duration::ZERO);
        assert_eq!(progress.relative_path(), path);
    }

    #[test]
    fn bytes_transferred_returns_value() {
        let progress =
            LocalCopyProgress::new(Path::new("test.txt"), 12345, None, Duration::ZERO);
        assert_eq!(progress.bytes_transferred(), 12345);
    }

    #[test]
    fn total_bytes_some() {
        let progress =
            LocalCopyProgress::new(Path::new("test.txt"), 50, Some(100), Duration::ZERO);
        assert_eq!(progress.total_bytes(), Some(100));
    }

    #[test]
    fn total_bytes_none() {
        let progress = LocalCopyProgress::new(Path::new("test.txt"), 50, None, Duration::ZERO);
        assert_eq!(progress.total_bytes(), None);
    }

    #[test]
    fn elapsed_returns_duration() {
        let duration = Duration::from_millis(500);
        let progress = LocalCopyProgress::new(Path::new("test.txt"), 0, None, duration);
        assert_eq!(progress.elapsed(), duration);
    }

    #[test]
    fn clone_works() {
        let progress =
            LocalCopyProgress::new(Path::new("test.txt"), 100, Some(200), Duration::from_secs(1));
        let cloned = progress;
        assert_eq!(cloned.bytes_transferred(), 100);
    }

    #[test]
    fn debug_format() {
        let progress = LocalCopyProgress::new(Path::new("test.txt"), 0, None, Duration::ZERO);
        let debug = format!("{:?}", progress);
        assert!(debug.contains("LocalCopyProgress"));
    }
}
