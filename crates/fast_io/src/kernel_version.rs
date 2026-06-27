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
pub fn parse_kernel_version(release: &str) -> Option<KernelVersion> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some(KernelVersion { major, minor })
}

/// Minimum kernel version required for io_uring (5.6).
pub const IO_URING_MIN_KERNEL: KernelVersion = KernelVersion { major: 5, minor: 6 };

/// Trait for kernel features that have a minimum version requirement.
///
/// Reduces maintenance surface by centralising the version-check pattern
/// that otherwise gets duplicated across every kernel-gated feature. Each
/// implementor declares its minimum kernel version and a human-readable
/// feature name; the trait provides a default `is_supported` method that
/// compares against a given `KernelVersion`.
pub trait VersionRequirement {
    /// Minimum kernel version required for this feature.
    fn min_version(&self) -> KernelVersion;

    /// Human-readable name for diagnostic messages (e.g. "io_uring",
    /// "PBUF_RING", "LINKAT").
    fn feature_name(&self) -> &str;

    /// Returns `true` if `kernel` meets the minimum version requirement.
    fn is_supported(&self, kernel: &KernelVersion) -> bool {
        let min = self.min_version();
        kernel.meets_minimum(min.major, min.minor)
    }

    /// Returns a diagnostic string indicating whether the feature is
    /// supported on the given kernel, or why not.
    fn check_message(&self, kernel: &KernelVersion) -> String {
        let min = self.min_version();
        if self.is_supported(kernel) {
            format!(
                "{}: supported (kernel {} >= {})",
                self.feature_name(),
                kernel,
                min
            )
        } else {
            format!(
                "{}: unsupported (kernel {} < {} required)",
                self.feature_name(),
                kernel,
                min
            )
        }
    }
}

/// io_uring basic availability (Linux 5.6+).
pub struct IoUringRequirement;

impl VersionRequirement for IoUringRequirement {
    fn min_version(&self) -> KernelVersion {
        IO_URING_MIN_KERNEL
    }
    fn feature_name(&self) -> &str {
        "io_uring"
    }
}

/// PBUF_RING provided-buffer ring (Linux 5.19+).
pub struct PbufRingRequirement;

impl VersionRequirement for PbufRingRequirement {
    fn min_version(&self) -> KernelVersion {
        KernelVersion {
            major: 5,
            minor: 19,
        }
    }
    fn feature_name(&self) -> &str {
        "PBUF_RING"
    }
}

/// IORING_OP_LINKAT (Linux 5.15+).
pub struct LinkatRequirement;

impl VersionRequirement for LinkatRequirement {
    fn min_version(&self) -> KernelVersion {
        KernelVersion {
            major: 5,
            minor: 15,
        }
    }
    fn feature_name(&self) -> &str {
        "LINKAT"
    }
}

/// IORING_OP_STATX and IORING_OP_RENAMEAT (Linux 5.11+).
pub struct StatxRenameatRequirement;

impl VersionRequirement for StatxRenameatRequirement {
    fn min_version(&self) -> KernelVersion {
        KernelVersion {
            major: 5,
            minor: 11,
        }
    }
    fn feature_name(&self) -> &str {
        "STATX/RENAMEAT"
    }
}

/// IORING_OP_SEND_ZC zero-copy socket send (Linux 6.0+).
pub struct SendZcRequirement;

impl VersionRequirement for SendZcRequirement {
    fn min_version(&self) -> KernelVersion {
        KernelVersion { major: 6, minor: 0 }
    }
    fn feature_name(&self) -> &str {
        "SEND_ZC"
    }
}

/// `RWF_DONTCACHE` uncached buffered I/O (Linux 6.14+).
pub struct DontcacheRequirement;

impl VersionRequirement for DontcacheRequirement {
    fn min_version(&self) -> KernelVersion {
        KernelVersion {
            major: 6,
            minor: 14,
        }
    }
    fn feature_name(&self) -> &str {
        "RWF_DONTCACHE"
    }
}

/// Logs the io_uring probe result at debug level.
///
/// On Linux with the `io_uring` feature enabled, this queries the kernel
/// version and io_uring availability, emitting a `debug_log!(Io, 1, ...)`
/// message describing the result. When io_uring is unavailable, also logs
/// the specific restriction type (seccomp, cgroup, kernel version) so
/// operators can diagnose container or cloud environment issues.
///
/// Callers should invoke this once during startup after the io_uring probe
/// has been cached.
///
/// On non-Linux platforms or without the `io_uring` feature, this is a no-op.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub fn log_io_uring_probe_result() {
    let reason = crate::io_uring_availability_reason();
    logging::debug_log!(Io, 1, "{reason}");

    let restriction = crate::detect_io_uring_restriction();
    if restriction != crate::IoUringRestriction::None {
        logging::debug_log!(Io, 1, "io_uring restriction: {restriction}");
    }
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

    #[test]
    fn version_requirement_io_uring_matches_constant() {
        let req = IoUringRequirement;
        assert_eq!(req.min_version(), IO_URING_MIN_KERNEL);
        assert_eq!(req.feature_name(), "io_uring");
    }

    #[test]
    fn version_requirement_is_supported_at_exact_minimum() {
        let req = IoUringRequirement;
        let v = KernelVersion { major: 5, minor: 6 };
        assert!(req.is_supported(&v));
    }

    #[test]
    fn version_requirement_is_not_supported_below_minimum() {
        let req = IoUringRequirement;
        let v = KernelVersion { major: 5, minor: 5 };
        assert!(!req.is_supported(&v));
    }

    #[test]
    fn version_requirement_is_supported_above_minimum() {
        let req = IoUringRequirement;
        let v = KernelVersion { major: 6, minor: 1 };
        assert!(req.is_supported(&v));
    }

    #[test]
    fn version_requirement_check_message_supported() {
        let req = IoUringRequirement;
        let v = KernelVersion { major: 6, minor: 1 };
        let msg = req.check_message(&v);
        assert!(msg.contains("supported"), "got: {msg}");
        assert!(msg.contains("io_uring"), "got: {msg}");
        assert!(msg.contains("6.1"), "got: {msg}");
    }

    #[test]
    fn version_requirement_check_message_unsupported() {
        let req = IoUringRequirement;
        let v = KernelVersion {
            major: 4,
            minor: 19,
        };
        let msg = req.check_message(&v);
        assert!(msg.contains("unsupported"), "got: {msg}");
        assert!(msg.contains("4.19"), "got: {msg}");
        assert!(msg.contains("5.6"), "got: {msg}");
    }

    #[test]
    fn pbuf_ring_requirement_needs_5_19() {
        let req = PbufRingRequirement;
        assert_eq!(
            req.min_version(),
            KernelVersion {
                major: 5,
                minor: 19
            }
        );
        assert!(!req.is_supported(&KernelVersion {
            major: 5,
            minor: 18
        }));
        assert!(req.is_supported(&KernelVersion {
            major: 5,
            minor: 19
        }));
    }

    #[test]
    fn send_zc_requirement_needs_6_0() {
        let req = SendZcRequirement;
        assert_eq!(req.min_version(), KernelVersion { major: 6, minor: 0 });
        assert!(!req.is_supported(&KernelVersion {
            major: 5,
            minor: 19
        }));
        assert!(req.is_supported(&KernelVersion { major: 6, minor: 0 }));
    }

    #[test]
    fn dontcache_requirement_needs_6_14() {
        let req = DontcacheRequirement;
        assert_eq!(
            req.min_version(),
            KernelVersion {
                major: 6,
                minor: 14
            }
        );
        assert!(!req.is_supported(&KernelVersion {
            major: 6,
            minor: 13
        }));
        assert!(req.is_supported(&KernelVersion {
            major: 6,
            minor: 14
        }));
    }

    #[test]
    fn linkat_requirement_needs_5_15() {
        let req = LinkatRequirement;
        assert_eq!(
            req.min_version(),
            KernelVersion {
                major: 5,
                minor: 15
            }
        );
        assert!(!req.is_supported(&KernelVersion {
            major: 5,
            minor: 14
        }));
        assert!(req.is_supported(&KernelVersion {
            major: 5,
            minor: 15
        }));
    }

    #[test]
    fn statx_renameat_requirement_needs_5_11() {
        let req = StatxRenameatRequirement;
        assert_eq!(
            req.min_version(),
            KernelVersion {
                major: 5,
                minor: 11
            }
        );
        assert!(!req.is_supported(&KernelVersion {
            major: 5,
            minor: 10
        }));
        assert!(req.is_supported(&KernelVersion {
            major: 5,
            minor: 11
        }));
    }
}
