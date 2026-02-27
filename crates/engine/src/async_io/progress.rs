//! Progress and result types for async copy operations.

use std::path::PathBuf;
use std::time::Duration;

/// Progress information for async file copy operations.
#[derive(Debug, Clone)]
pub struct CopyProgress {
    /// Total bytes copied so far.
    pub bytes_copied: u64,
    /// Total size of the source file.
    pub total_bytes: u64,
    /// Elapsed time since copy started.
    pub elapsed: Duration,
    /// Source file path.
    pub source: PathBuf,
    /// Destination file path.
    pub destination: PathBuf,
}

impl CopyProgress {
    /// Returns the copy progress as a percentage (0.0 to 100.0).
    #[must_use]
    pub fn percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            100.0
        } else {
            (self.bytes_copied as f64 / self.total_bytes as f64) * 100.0
        }
    }

    /// Returns the current transfer rate in bytes per second.
    #[must_use]
    pub fn bytes_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.bytes_copied as f64 / secs
        } else {
            0.0
        }
    }
}

/// Result of an async file copy operation.
#[derive(Debug, Clone)]
pub struct CopyResult {
    /// Total bytes copied.
    pub bytes_copied: u64,
    /// Total time elapsed.
    pub elapsed: Duration,
    /// Source file path.
    pub source: PathBuf,
    /// Destination file path.
    pub destination: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_progress_percentage() {
        let progress = CopyProgress {
            bytes_copied: 50,
            total_bytes: 100,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.percentage() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_bytes_per_second() {
        let progress = CopyProgress {
            bytes_copied: 1000,
            total_bytes: 2000,
            elapsed: Duration::from_secs(2),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.bytes_per_second() - 500.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_percentage_zero_total() {
        let progress = CopyProgress {
            bytes_copied: 0,
            total_bytes: 0,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.percentage() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_bytes_per_second_zero_elapsed() {
        let progress = CopyProgress {
            bytes_copied: 1000,
            total_bytes: 2000,
            elapsed: Duration::from_secs(0),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.bytes_per_second() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_clone() {
        let progress = CopyProgress {
            bytes_copied: 100,
            total_bytes: 200,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let cloned = progress.clone();
        assert_eq!(cloned.bytes_copied, progress.bytes_copied);
        assert_eq!(cloned.total_bytes, progress.total_bytes);
    }

    #[test]
    fn test_copy_progress_debug() {
        let progress = CopyProgress {
            bytes_copied: 100,
            total_bytes: 200,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let debug = format!("{progress:?}");
        assert!(debug.contains("CopyProgress"));
    }

    #[test]
    fn test_copy_result_clone() {
        let result = CopyResult {
            bytes_copied: 100,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let cloned = result.clone();
        assert_eq!(cloned.bytes_copied, result.bytes_copied);
    }

    #[test]
    fn test_copy_result_debug() {
        let result = CopyResult {
            bytes_copied: 100,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("CopyResult"));
    }
}
