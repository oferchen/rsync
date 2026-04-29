//! Kernel version parsing and io_uring probe logging.
//!
//! Provides a portable kernel version parser and a log-emitting probe function
//! that reports whether io_uring is available and why. On non-Linux platforms
//! or without the `io_uring` feature, the probe function is a no-op.

/// Parsed kernel version with major and minor components.
///
/// Extracted from a `uname -r` style release string such as `"5.15.0-generic"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelVersion {
    /// Major version number (e.g., 5 in "5.15.0").
    pub major: u32,
    /// Minor version number (e.g., 15 in "5.15.0").
    pub minor: u32,
}

impl KernelVersion {
    /// Returns `true` if this version meets or exceeds the given minimum.
    ///
    /// # Examples
    ///
    /// ```
    /// use fast_io::kernel_version::KernelVersion;
    ///
    /// let v = KernelVersion { major: 5, minor: 15 };
    /// assert!(v.meets_minimum(5, 6));
    /// assert!(!v.meets_minimum(6, 0));
    /// ```
    #[must_use]
    pub fn meets_minimum(&self, min_major: u32, min_minor: u32) -> bool {
        (self.major, self.minor) >= (min_major, min_minor)
    }
}

impl std::fmt::Display for KernelVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Parses a kernel version from a `uname -r` style release string.
///
/// Extracts the leading `major.minor` numeric components, ignoring any
/// trailing patch level, distribution suffix, or build metadata.
///
/// Returns `None` if the string does not contain at least two dot-separated
/// numeric components.
///
/// # Examples
///
/// ```
/// use fast_io::kernel_version::{KernelVersion, parse_kernel_version};
///
/// assert_eq!(
///     parse_kernel_version("5.15.0-generic"),
///     Some(KernelVersion { major: 5, minor: 15 })
/// );
/// assert_eq!(
///     parse_kernel_version("6.1.0-rc1"),
///     Some(KernelVersion { major: 6, minor: 1 })
/// );
/// assert_eq!(parse_kernel_version("invalid"), None);
/// ```
#[must_use]
pub fn parse_kernel_version(release: &str) -> Option<KernelVersion> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some(KernelVersion { major, minor })
}

/// Minimum kernel version required for io_uring (5.6).
pub const IO_URING_MIN_KERNEL: KernelVersion = KernelVersion { major: 5, minor: 6 };

/// Logs the io_uring probe result at debug level.
///
/// On Linux with the `io_uring` feature enabled, this queries the kernel
/// version and io_uring availability, emitting a `debug_log!(Io, 1, ...)`
/// message describing the result. Callers should invoke this once during
/// startup after the io_uring probe has been cached.
///
/// On non-Linux platforms or without the `io_uring` feature, this is a no-op.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub fn log_io_uring_probe_result() {
    let reason = crate::io_uring_availability_reason();
    logging::debug_log!(Io, 1, "{reason}");
}

/// No-op stub for non-Linux platforms or when io_uring feature is disabled.
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
pub fn log_io_uring_probe_result() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_release() {
        assert_eq!(
            parse_kernel_version("5.15.0"),
            Some(KernelVersion {
                major: 5,
                minor: 15
            })
        );
    }

    #[test]
    fn parse_with_generic_suffix() {
        assert_eq!(
            parse_kernel_version("5.15.0-generic"),
            Some(KernelVersion {
                major: 5,
                minor: 15
            })
        );
    }

    #[test]
    fn parse_release_candidate() {
        assert_eq!(
            parse_kernel_version("6.1.0-rc1"),
            Some(KernelVersion { major: 6, minor: 1 })
        );
    }

    #[test]
    fn parse_azure_style() {
        assert_eq!(
            parse_kernel_version("5.15.0.1-azure"),
            Some(KernelVersion {
                major: 5,
                minor: 15
            })
        );
    }

    #[test]
    fn parse_wsl2_style() {
        assert_eq!(
            parse_kernel_version("5.15.167.4-microsoft-standard-WSL2"),
            Some(KernelVersion {
                major: 5,
                minor: 15
            })
        );
    }

    #[test]
    fn parse_chromeos_style() {
        assert_eq!(
            parse_kernel_version("5.10.159-20950-g5765b1ef511a"),
            Some(KernelVersion {
                major: 5,
                minor: 10
            })
        );
    }

    #[test]
    fn parse_aws_style() {
        assert_eq!(
            parse_kernel_version("4.19.123-aws"),
            Some(KernelVersion {
                major: 4,
                minor: 19
            })
        );
    }

    #[test]
    fn parse_large_version_numbers() {
        assert_eq!(
            parse_kernel_version("100.200.300"),
            Some(KernelVersion {
                major: 100,
                minor: 200
            })
        );
    }

    #[test]
    fn parse_leading_zeros() {
        assert_eq!(
            parse_kernel_version("06.01.00"),
            Some(KernelVersion { major: 6, minor: 1 })
        );
    }

    #[test]
    fn parse_zero_zero() {
        assert_eq!(
            parse_kernel_version("0.0.0"),
            Some(KernelVersion { major: 0, minor: 0 })
        );
    }

    #[test]
    fn parse_minimum_io_uring_version() {
        assert_eq!(
            parse_kernel_version("5.6.0"),
            Some(KernelVersion { major: 5, minor: 6 })
        );
    }

    #[test]
    fn parse_empty_string() {
        assert_eq!(parse_kernel_version(""), None);
    }

    #[test]
    fn parse_non_numeric() {
        assert_eq!(parse_kernel_version("invalid"), None);
    }

    #[test]
    fn parse_single_digit() {
        assert_eq!(parse_kernel_version("5"), None);
    }

    #[test]
    fn parse_only_dots() {
        assert_eq!(parse_kernel_version("..."), None);
    }

    #[test]
    fn parse_letters_only() {
        assert_eq!(parse_kernel_version("abc.def.ghi"), None);
    }

    #[test]
    fn meets_minimum_exact_match() {
        let v = KernelVersion { major: 5, minor: 6 };
        assert!(v.meets_minimum(5, 6));
    }

    #[test]
    fn meets_minimum_higher_major() {
        let v = KernelVersion { major: 6, minor: 0 };
        assert!(v.meets_minimum(5, 6));
    }

    #[test]
    fn meets_minimum_higher_minor() {
        let v = KernelVersion {
            major: 5,
            minor: 15,
        };
        assert!(v.meets_minimum(5, 6));
    }

    #[test]
    fn meets_minimum_lower_major() {
        let v = KernelVersion {
            major: 4,
            minor: 19,
        };
        assert!(!v.meets_minimum(5, 6));
    }

    #[test]
    fn meets_minimum_same_major_lower_minor() {
        let v = KernelVersion { major: 5, minor: 5 };
        assert!(!v.meets_minimum(5, 6));
    }

    #[test]
    fn display_format() {
        let v = KernelVersion {
            major: 5,
            minor: 15,
        };
        assert_eq!(format!("{v}"), "5.15");
    }

    #[test]
    fn min_kernel_is_5_6() {
        assert_eq!(IO_URING_MIN_KERNEL.major, 5);
        assert_eq!(IO_URING_MIN_KERNEL.minor, 6);
    }

    #[test]
    fn min_kernel_meets_itself() {
        assert!(IO_URING_MIN_KERNEL.meets_minimum(5, 6));
    }

    #[test]
    fn log_probe_result_does_not_panic() {
        // On all platforms, calling the probe log function must not panic.
        // On non-Linux, this is a no-op. On Linux, it emits a debug_log message.
        log_io_uring_probe_result();
    }
}
