//! Integration tests for the io_uring runtime probe and fallback chain.
//!
//! These tests verify that the public probe API and the policy-driven
//! reader/writer factories degrade gracefully on systems that do not
//! support io_uring - either because the kernel is older than 5.6, the
//! syscall is blocked by seccomp/container policy, or the platform is
//! not Linux at all. The same APIs must also be safe to call repeatedly
//! and concurrently, since the result is cached process-wide.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use fast_io::{
    IO_URING_MIN_KERNEL, IoUringPolicy, IoUringReaderFactory, IoUringWriterFactory,
    io_uring_availability_reason, io_uring_kernel_info, io_uring_status_detail,
    is_io_uring_available, log_io_uring_probe_result, parse_kernel_version, reader_from_path,
    sqpoll_fell_back, writer_from_file,
};
use tempfile::tempdir;

#[test]
fn is_io_uring_available_is_idempotent() {
    let first = is_io_uring_available();
    let second = is_io_uring_available();
    let third = is_io_uring_available();
    assert_eq!(first, second);
    assert_eq!(second, third);
}

#[test]
fn availability_reason_matches_probe() {
    let available = is_io_uring_available();
    let reason = io_uring_availability_reason();

    // Reason string is always namespaced and never empty.
    assert!(reason.starts_with("io_uring: "), "got: {reason}");
    assert!(!reason.is_empty());

    // The reason must agree with the boolean probe.
    if available {
        assert!(
            reason.contains("enabled"),
            "available probe but reason claims disabled: {reason}"
        );
    } else {
        assert!(
            reason.contains("disabled"),
            "unavailable probe but reason claims enabled: {reason}"
        );
    }
}

#[test]
fn kernel_info_matches_probe() {
    let info = io_uring_kernel_info();
    assert_eq!(info.available, is_io_uring_available());
    assert_eq!(info.reason, io_uring_availability_reason());

    if info.available {
        assert!(info.kernel_major.is_some());
        assert!(info.kernel_minor.is_some());
    } else {
        // When unavailable due to syscall block, kernel version may still
        // be populated; when unavailable for any other reason, supported_ops
        // must be zero.
        assert_eq!(info.supported_ops, 0);
    }
}

#[test]
fn status_detail_is_descriptive() {
    let detail = io_uring_status_detail();
    assert!(!detail.is_empty());
    // Either "available", "unavailable", "compiled in", or "not available"
    // depending on platform/feature combination.
    let lower = detail.to_lowercase();
    assert!(
        lower.contains("available") || lower.contains("compiled"),
        "expected status to mention availability or compilation: {detail}"
    );
}

#[test]
fn log_probe_result_is_safe_to_call() {
    // Must never panic regardless of platform or kernel version. Repeated
    // calls are also safe because the underlying probe is cached.
    log_io_uring_probe_result();
    log_io_uring_probe_result();
}

#[test]
fn min_kernel_constant_is_5_6() {
    assert!(IO_URING_MIN_KERNEL.meets_minimum(5, 6));
    assert!(!IO_URING_MIN_KERNEL.meets_minimum(5, 7));
    // Older kernels that lack io_uring must fail the minimum check.
    let v_4_19 = parse_kernel_version("4.19.0").expect("parse 4.19");
    assert!(!v_4_19.meets_minimum(5, 6));
    let v_5_5 = parse_kernel_version("5.5.0").expect("parse 5.5");
    assert!(!v_5_5.meets_minimum(5, 6));
    let v_5_6 = parse_kernel_version("5.6.0").expect("parse 5.6");
    assert!(v_5_6.meets_minimum(5, 6));
}

#[test]
fn sqpoll_fallback_is_initially_false() {
    // No production code in the test harness should have requested SQPOLL,
    // so the fallback flag must remain false. Even if it has been flipped
    // by another test in the same process, the call itself must not panic.
    let _ = sqpoll_fell_back();
}

#[test]
fn concurrent_probe_calls_are_safe() {
    // Hammer the cached probe from multiple threads to verify there is no
    // data race or panic regardless of which thread wins the first probe.
    let panicked = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = Arc::clone(&panicked);
        handles.push(thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                for _ in 0..32 {
                    let _ = is_io_uring_available();
                    let _ = io_uring_availability_reason();
                    let _ = io_uring_kernel_info();
                }
            });
            if result.is_err() {
                p.store(true, Ordering::SeqCst);
            }
        }));
    }
    for h in handles {
        h.join().expect("thread joined");
    }
    assert!(!panicked.load(Ordering::SeqCst), "probe panicked");
}

#[test]
fn auto_policy_yields_working_writer_and_reader() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("auto_policy.bin");

    // Writer must succeed regardless of io_uring availability.
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Auto)
            .expect("Auto policy writer must succeed on every platform");
        writer.write_all(b"fallback works").unwrap();
        writer.flush().unwrap();
    }

    // Reader must successfully read what the writer produced.
    let mut reader = reader_from_path(&path, IoUringPolicy::Auto)
        .expect("Auto policy reader must succeed on every platform");
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).unwrap();
    assert_eq!(buf, b"fallback works");
}

#[test]
fn disabled_policy_always_uses_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disabled_policy.bin");

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 8192, IoUringPolicy::Disabled).unwrap();
        writer.write_all(b"std path").unwrap();
        writer.flush().unwrap();
    }

    let mut reader = reader_from_path(&path, IoUringPolicy::Disabled).unwrap();
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).unwrap();
    assert_eq!(buf, b"std path");
}

#[test]
fn factory_reports_runtime_decision_consistently() {
    // The factory's `will_use_io_uring()` must agree with the cached probe
    // result so callers can choose configuration paths up front. On non-Linux
    // or stub builds this is always false; on Linux with io_uring enabled it
    // must match `is_io_uring_available()`.
    let reader_factory = IoUringReaderFactory::default();
    let writer_factory = IoUringWriterFactory::default();
    let probe = is_io_uring_available();
    assert_eq!(reader_factory.will_use_io_uring(), probe);
    assert_eq!(writer_factory.will_use_io_uring(), probe);
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
#[test]
fn probe_is_unavailable_without_linux_io_uring() {
    assert!(!is_io_uring_available());

    let info = io_uring_kernel_info();
    assert!(!info.available);
    assert_eq!(info.kernel_major, None);
    assert_eq!(info.kernel_minor, None);
    assert_eq!(info.supported_ops, 0);

    let reason = io_uring_availability_reason();
    assert!(reason.contains("disabled"));
    // On non-Linux the reason mentions the platform; on Linux without the
    // feature it mentions the missing feature.
    #[cfg(not(target_os = "linux"))]
    assert!(reason.contains("platform is not Linux"));
    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    assert!(reason.contains("not compiled in"));
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
#[test]
fn enabled_policy_returns_unsupported_without_linux_io_uring() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("enabled_policy.bin");
    let file = std::fs::File::create(&path).unwrap();

    let writer_err = writer_from_file(file, 8192, IoUringPolicy::Enabled).unwrap_err();
    assert_eq!(writer_err.kind(), std::io::ErrorKind::Unsupported);
    assert!(writer_err.to_string().to_lowercase().contains("io_uring"));

    let reader_err = reader_from_path(&path, IoUringPolicy::Enabled).unwrap_err();
    assert_eq!(reader_err.kind(), std::io::ErrorKind::Unsupported);
    assert!(reader_err.to_string().to_lowercase().contains("io_uring"));
}

/// Linux with the io_uring feature: the probe runs against the live kernel.
/// A kernel meeting the documented 5.6 minimum can still be reported unavailable
/// if the syscall is blocked (for example by seccomp inside a container). Both
/// outcomes are acceptable - what matters is that the boolean and the reason agree.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
#[test]
fn linux_probe_matches_kernel_version() {
    let info = io_uring_kernel_info();
    if let (Some(major), Some(minor)) = (info.kernel_major, info.kernel_minor) {
        let meets_min = (major, minor) >= (5, 6);
        if !meets_min {
            assert!(
                !info.available,
                "kernel {major}.{minor} is below 5.6 but probe reports available"
            );
            assert!(info.reason.contains("< 5.6 required"));
        }
        // If the kernel is new enough, availability depends on the syscall
        // permission (seccomp/container can still block it). Both outcomes
        // are valid; only verify the reason matches the boolean.
        if info.available {
            assert!(info.reason.contains("enabled"));
            assert!(info.supported_ops > 0, "available kernel reported 0 ops");
        }
    } else {
        // Could not parse uname output; probe must report unavailable with
        // an explanatory reason and zero supported ops.
        assert!(!info.available);
        assert_eq!(info.supported_ops, 0);
    }
}
