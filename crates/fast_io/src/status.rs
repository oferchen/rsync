//! Runtime status and capability reporting for `--version` output.
//!
//! Combines compile-time platform information with runtime probes for
//! io_uring and IOCP into human-readable strings suitable for CLI output
//! and structured records for programmatic callers.

use crate::io_uring;
#[cfg(target_os = "linux")]
use crate::io_uring::is_io_uring_available;
#[cfg(target_os = "windows")]
use crate::iocp::is_iocp_available;
#[cfg(all(target_os = "windows", feature = "iocp"))]
use crate::iocp::skip_event_optimization_available;
#[cfg(target_os = "linux")]
use crate::splice::is_splice_available;

/// Detailed IOCP availability status for `--version` output.
///
/// Returns a human-readable string describing IOCP support:
/// - Whether the feature was compiled in
/// - Whether the OS supports it (Windows only)
#[must_use]
pub fn iocp_status_detail() -> String {
    iocp_status_detail_impl()
}

#[cfg(all(target_os = "windows", feature = "iocp"))]
fn iocp_status_detail_impl() -> String {
    if is_iocp_available() {
        let skip_event = if skip_event_optimization_available() {
            ", FILE_SKIP_SET_EVENT_ON_HANDLE active"
        } else {
            ""
        };
        format!("compiled in, available{skip_event}")
    } else {
        "compiled in, unavailable (CreateIoCompletionPort failed)".to_string()
    }
}

#[cfg(not(all(target_os = "windows", feature = "iocp")))]
fn iocp_status_detail_impl() -> String {
    #[cfg(not(target_os = "windows"))]
    {
        "not available (platform is not Windows)".to_string()
    }
    #[cfg(all(target_os = "windows", not(feature = "iocp")))]
    {
        "not compiled in (iocp feature disabled)".to_string()
    }
}

/// Detailed io_uring availability status for `--version` output.
///
/// Returns a human-readable string describing io_uring support:
/// - Whether the feature was compiled in
/// - Whether the kernel supports it (Linux only)
/// - The detected kernel version when relevant
#[must_use]
pub fn io_uring_status_detail() -> String {
    io_uring_status_detail_impl()
}

/// Returns a log-friendly reason string for io_uring availability.
///
/// On Linux with the `io_uring` feature enabled, probes the kernel version
/// and attempts `io_uring_setup(2)`, returning a message like:
/// - `"io_uring: enabled (kernel 5.15, 48 ops supported)"`
/// - `"io_uring: disabled (kernel 4.19 < 5.6 required)"`
/// - `"io_uring: disabled (kernel 6.1, io_uring_setup(2) blocked by seccomp, container, or permission restriction)"`
///
/// On non-Linux platforms or without the feature, returns a compile-time reason.
#[must_use]
pub fn io_uring_availability_reason() -> String {
    io_uring_availability_reason_impl()
}

/// Returns structured kernel information for io_uring availability.
///
/// Provides machine-readable fields for callers that need to act on
/// kernel version or supported op count programmatically. On non-Linux
/// platforms or without the `io_uring` feature, returns a struct with
/// `available: false` and `None` kernel versions.
#[must_use]
pub fn io_uring_kernel_info() -> io_uring::IoUringKernelInfo {
    io_uring_kernel_info_impl()
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_availability_reason_impl() -> String {
    io_uring::config_detail::io_uring_availability_reason()
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn io_uring_availability_reason_impl() -> String {
    #[cfg(not(target_os = "linux"))]
    {
        "io_uring: disabled (platform is not Linux)".to_string()
    }
    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    {
        "io_uring: disabled (io_uring feature not compiled in)".to_string()
    }
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_kernel_info_impl() -> io_uring::IoUringKernelInfo {
    io_uring::config_detail::io_uring_kernel_info()
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn io_uring_kernel_info_impl() -> io_uring::IoUringKernelInfo {
    io_uring::IoUringKernelInfo {
        available: false,
        kernel_major: None,
        kernel_minor: None,
        supported_ops: 0,
        pbuf_ring_supported: false,
        reason: io_uring_availability_reason_impl(),
    }
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_status_detail_impl() -> String {
    let info = io_uring::config_detail::io_uring_kernel_info();

    match (info.kernel_major, info.kernel_minor) {
        (Some(major), Some(minor)) => {
            if info.available {
                format!(
                    "compiled in, available (kernel {major}.{minor}, {} ops)",
                    info.supported_ops
                )
            } else {
                format!("compiled in, unavailable (kernel {major}.{minor}, requires >= 5.6)")
            }
        }
        _ => "compiled in, unavailable (could not detect kernel version)".to_string(),
    }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn io_uring_status_detail_impl() -> String {
    #[cfg(not(target_os = "linux"))]
    {
        "not available (platform is not Linux)".to_string()
    }
    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    {
        "not compiled in (io_uring feature disabled)".to_string()
    }
}

/// Returns the platform I/O capabilities available on this system.
///
/// Each entry is a human-readable label describing an available fast I/O path.
/// Compile-time capabilities (determined by target OS) are always included when
/// applicable. Runtime-probed capabilities (io_uring, splice) are included only
/// when the probe succeeds.
///
/// # Platform-specific entries
///
/// - **Linux**: `copy_file_range`, `sendfile`, `splice` (runtime-probed),
///   `FICLONE`, `O_TMPFILE`, `io_uring` (runtime-probed)
/// - **macOS**: `clonefile`, `fcopyfile`, `F_NOCACHE`, `writev`
/// - **Windows**: `CopyFileEx`, `ReFS reflink`, `IOCP` (runtime-probed)
#[must_use]
pub fn platform_io_capabilities() -> Vec<&'static str> {
    let mut caps = Vec::new();

    // Linux compile-time capabilities
    #[cfg(target_os = "linux")]
    {
        caps.push("copy_file_range");
        caps.push("sendfile");

        if is_splice_available() {
            caps.push("splice");
        }

        caps.push("FICLONE");
        caps.push("O_TMPFILE");

        if is_io_uring_available() {
            caps.push("io_uring");
        }
    }

    // macOS compile-time capabilities
    #[cfg(target_os = "macos")]
    {
        caps.push("clonefile");
        caps.push("fcopyfile");
        caps.push("F_NOCACHE");
        caps.push("writev");
    }

    // Windows compile-time capabilities
    #[cfg(target_os = "windows")]
    {
        caps.push("CopyFileEx");
        caps.push("ReFS reflink");
        if is_iocp_available() {
            caps.push("IOCP");
        }
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_io_capabilities_returns_expected_entries() {
        let caps = platform_io_capabilities();

        #[cfg(target_os = "linux")]
        {
            assert!(caps.contains(&"copy_file_range"));
            assert!(caps.contains(&"sendfile"));
            assert!(caps.contains(&"FICLONE"));
            assert!(caps.contains(&"O_TMPFILE"));
        }

        #[cfg(target_os = "macos")]
        {
            assert!(caps.contains(&"clonefile"));
            assert!(caps.contains(&"fcopyfile"));
            assert!(caps.contains(&"F_NOCACHE"));
            assert!(caps.contains(&"writev"));
        }

        #[cfg(target_os = "windows")]
        {
            assert!(caps.contains(&"CopyFileEx"));
            assert!(caps.contains(&"ReFS reflink"));
        }
    }

    #[test]
    fn iocp_status_detail_returns_non_empty_string() {
        let detail = iocp_status_detail();
        assert!(!detail.is_empty());

        #[cfg(not(target_os = "windows"))]
        assert!(detail.contains("not available"));

        #[cfg(all(target_os = "windows", not(feature = "iocp")))]
        assert!(detail.contains("not compiled in"));

        #[cfg(all(target_os = "windows", feature = "iocp"))]
        assert!(detail.contains("compiled in"));
    }

    #[test]
    fn iocp_status_detail_is_single_line() {
        let detail = iocp_status_detail();
        assert!(!detail.contains('\n'));
    }

    #[test]
    fn iocp_status_detail_no_trailing_whitespace() {
        let detail = iocp_status_detail();
        assert_eq!(detail, detail.trim());
    }

    #[test]
    fn io_uring_status_detail_returns_non_empty_string() {
        let detail = io_uring_status_detail();
        assert!(!detail.is_empty());

        #[cfg(not(target_os = "linux"))]
        assert!(detail.contains("not available"));

        #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
        assert!(detail.contains("not compiled in"));

        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        assert!(detail.contains("compiled in"));
    }

    #[test]
    fn io_uring_availability_reason_returns_non_empty_string() {
        let reason = io_uring_availability_reason();
        assert!(!reason.is_empty());
        assert!(reason.starts_with("io_uring: "));

        #[cfg(not(target_os = "linux"))]
        assert!(reason.contains("not Linux"));

        #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
        assert!(reason.contains("not compiled in"));

        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        {
            // Must contain either "enabled" or "disabled"
            assert!(reason.contains("enabled") || reason.contains("disabled"));
        }
    }

    #[test]
    fn platform_io_capabilities_has_no_duplicates() {
        let caps = platform_io_capabilities();
        let mut seen = std::collections::HashSet::new();
        for cap in &caps {
            assert!(seen.insert(cap), "duplicate capability: {cap}");
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_unavailable_on_non_linux() {
        assert!(
            !crate::is_io_uring_available(),
            "io_uring must not be available on non-Linux platforms"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_status_detail_indicates_platform_unavailability() {
        let detail = io_uring_status_detail();
        assert!(
            detail.contains("not available"),
            "status detail must indicate unavailability on non-Linux, got: {detail}"
        );
        assert!(
            detail.contains("not Linux"),
            "status detail must mention platform is not Linux, got: {detail}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_availability_reason_describes_platform_constraint() {
        let reason = io_uring_availability_reason();
        assert!(
            reason.starts_with("io_uring: disabled"),
            "reason must start with 'io_uring: disabled' on non-Linux, got: {reason}"
        );
        assert!(
            reason.contains("not Linux"),
            "reason must explain platform is not Linux, got: {reason}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_fallback_produces_no_errors() {
        // Verify that querying io_uring status on non-Linux does not panic or error -
        // the fallback path is exercised cleanly.
        let available = crate::is_io_uring_available();
        let detail = io_uring_status_detail();
        let reason = io_uring_availability_reason();

        assert!(!available);
        assert!(!detail.is_empty());
        assert!(!reason.is_empty());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_capabilities_excluded_on_non_linux() {
        let caps = platform_io_capabilities();
        assert!(
            !caps.contains(&"io_uring"),
            "io_uring must not appear in capabilities on non-Linux"
        );
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn io_uring_status_detail_well_formed_on_linux() {
        let detail = io_uring_status_detail();
        assert!(
            detail.starts_with("compiled in, "),
            "Linux+feature status must start with 'compiled in, ', got: {detail}"
        );
        assert!(
            detail.contains("available") || detail.contains("unavailable"),
            "status detail must indicate availability state, got: {detail}"
        );
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn io_uring_availability_reason_well_formed_on_linux() {
        let reason = io_uring_availability_reason();
        assert!(
            reason.starts_with("io_uring: "),
            "reason must start with 'io_uring: ', got: {reason}"
        );
        // On Linux with the feature, the reason must mention the kernel version
        // or a specific unavailability cause.
        let has_kernel_info = reason.contains("kernel");
        let has_parse_error = reason.contains("could not");
        assert!(
            has_kernel_info || has_parse_error,
            "reason must contain kernel info or parse error, got: {reason}"
        );
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn io_uring_availability_consistent_with_reason() {
        let available = is_io_uring_available();
        let reason = io_uring_availability_reason();

        if available {
            assert!(
                reason.contains("enabled"),
                "reason must say 'enabled' when io_uring is available, got: {reason}"
            );
            assert!(
                !reason.contains("disabled"),
                "reason must not say 'disabled' when io_uring is available, got: {reason}"
            );
        } else {
            assert!(
                reason.contains("disabled"),
                "reason must say 'disabled' when io_uring is not available, got: {reason}"
            );
        }
    }

    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    #[test]
    fn io_uring_feature_disabled_status() {
        let detail = io_uring_status_detail();
        assert!(
            detail.contains("not compiled in"),
            "status must indicate feature not compiled when io_uring feature disabled, got: {detail}"
        );

        let reason = io_uring_availability_reason();
        assert!(
            reason.contains("not compiled in"),
            "reason must indicate feature not compiled, got: {reason}"
        );
    }

    #[test]
    fn io_uring_status_detail_is_single_line() {
        let detail = io_uring_status_detail();
        assert!(
            !detail.contains('\n'),
            "status detail must be a single line for display purposes, got: {detail}"
        );
    }

    #[test]
    fn io_uring_availability_reason_is_single_line() {
        let reason = io_uring_availability_reason();
        assert!(
            !reason.contains('\n'),
            "availability reason must be a single line for log output, got: {reason}"
        );
    }

    #[test]
    fn io_uring_availability_reason_starts_with_io_uring_prefix() {
        let reason = io_uring_availability_reason();
        assert!(
            reason.starts_with("io_uring: "),
            "reason must start with 'io_uring: ' prefix for consistent log formatting, got: {reason}"
        );
    }

    #[test]
    fn io_uring_status_detail_no_trailing_whitespace() {
        let detail = io_uring_status_detail();
        assert_eq!(
            detail,
            detail.trim(),
            "status detail must not have leading/trailing whitespace"
        );
    }

    #[test]
    fn io_uring_availability_reason_no_trailing_whitespace() {
        let reason = io_uring_availability_reason();
        assert_eq!(
            reason,
            reason.trim(),
            "availability reason must not have leading/trailing whitespace"
        );
    }

    #[test]
    fn sqpoll_fell_back_starts_as_false() {
        // SQPOLL fallback flag must default to false - it is only set when
        // SQPOLL setup is attempted and fails on a Linux kernel.
        assert!(
            !crate::sqpoll_fell_back(),
            "sqpoll_fell_back() must be false at startup"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn factory_reader_falls_back_to_std_on_non_linux() {
        use crate::io_uring::{IoUringOrStdReader, IoUringReaderFactory};
        use crate::traits::FileReaderFactory;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory_fallback_reader.txt");
        std::fs::write(&path, b"fallback test content").unwrap();

        let factory = IoUringReaderFactory::default();
        assert!(
            !factory.will_use_io_uring(),
            "factory must not use io_uring on non-Linux"
        );

        let reader = factory.open(&path).unwrap();
        assert!(
            matches!(reader, IoUringOrStdReader::Std(_)),
            "reader must be Std variant on non-Linux"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn factory_writer_falls_back_to_std_on_non_linux() {
        use crate::io_uring::{IoUringOrStdWriter, IoUringWriterFactory};
        use crate::traits::FileWriterFactory;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory_fallback_writer.txt");

        let factory = IoUringWriterFactory::default();
        assert!(
            !factory.will_use_io_uring(),
            "factory must not use io_uring on non-Linux"
        );

        let writer = factory.create(&path).unwrap();
        assert!(
            matches!(writer, IoUringOrStdWriter::Std(_)),
            "writer must be Std variant on non-Linux"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn factory_writer_create_with_size_falls_back_to_std_on_non_linux() {
        use crate::io_uring::{IoUringOrStdWriter, IoUringWriterFactory};
        use crate::traits::FileWriterFactory;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory_fallback_sized.txt");

        let factory = IoUringWriterFactory::default();
        let writer = factory.create_with_size(&path, 4096).unwrap();
        assert!(
            matches!(writer, IoUringOrStdWriter::Std(_)),
            "sized writer must be Std variant on non-Linux"
        );
    }
}
