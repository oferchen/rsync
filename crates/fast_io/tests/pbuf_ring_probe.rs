//! Integration tests for the `IORING_REGISTER_PBUF_RING` runtime probe.
//!
//! These tests cover the cross-platform `pbuf_ring_supported()` re-export at
//! the crate root (and the matching `IoUringKernelInfo::pbuf_ring_supported`
//! field), exercising both the Linux + io_uring path and the non-Linux stub.
//!
//! On non-Linux platforms (or with the `io_uring` feature disabled) the probe
//! must always return `false`. On Linux with the feature enabled the probe is
//! cached process-wide via `OnceLock`, so multiple calls (including from many
//! threads) must agree without re-entering the kernel-version parser.
//!
//! See `docs/audits/iouring-pbuf-ring.md` for the full fallback chain
//! documentation: PBUF_RING (5.19+) -> classic provide-buffers (5.6+) ->
//! standard read/write -> non-Linux io_uring stub.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use fast_io::{io_uring_kernel_info, is_io_uring_available, pbuf_ring_supported};

#[test]
fn pbuf_ring_supported_is_idempotent() {
    let first = pbuf_ring_supported();
    let second = pbuf_ring_supported();
    let third = pbuf_ring_supported();
    assert_eq!(first, second);
    assert_eq!(second, third);
}

#[test]
fn pbuf_ring_supported_false_on_non_linux() {
    // On any non-Linux target, or when io_uring is compiled out, the probe
    // must report unsupported. PBUF_RING (kernel 5.19+) is a Linux-only
    // io_uring feature; the cross-platform stub returns `false`.
    #[cfg(not(all(target_os = "linux", feature = "io_uring")))]
    {
        assert!(
            !pbuf_ring_supported(),
            "stub probe must return false on non-Linux or feature-disabled builds"
        );
    }
    // On Linux with io_uring enabled, simply assert that the call succeeds
    // and is type-correct. The actual support depends on the kernel.
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    {
        let _: bool = pbuf_ring_supported();
    }
}

#[test]
fn pbuf_ring_supported_implies_io_uring_available() {
    // Logical invariant: PBUF_RING requires kernel 5.19+, which is strictly
    // stronger than the io_uring 5.6+ floor. If PBUF_RING is reported as
    // supported but io_uring itself is not, the layered probe is
    // inconsistent.
    if pbuf_ring_supported() {
        assert!(
            is_io_uring_available(),
            "PBUF_RING claimed without io_uring base support; layered probe is broken"
        );
    }
}

#[test]
fn kernel_info_pbuf_ring_field_agrees_with_probe() {
    let info = io_uring_kernel_info();
    assert_eq!(
        info.pbuf_ring_supported,
        pbuf_ring_supported(),
        "IoUringKernelInfo.pbuf_ring_supported must mirror the crate-root probe"
    );
}

#[test]
fn pbuf_ring_probe_is_cached_across_threads() {
    // The probe is cached behind `OnceLock`, so concurrent callers must all
    // see the same boolean. Spawn a fan-out that races the probe against
    // itself; the OnceLock contract guarantees at most one initialiser
    // runs.
    let expected = pbuf_ring_supported();
    let mismatched = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let mismatched = Arc::clone(&mismatched);
            thread::spawn(move || {
                for _ in 0..64 {
                    if pbuf_ring_supported() != expected {
                        mismatched.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("probe thread panicked");
    }

    assert!(
        !mismatched.load(Ordering::Relaxed),
        "OnceLock-cached probe returned divergent values across threads"
    );
}
