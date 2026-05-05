//! Integration tests covering the runtime SIMD level override.
//!
//! Each test in this binary mutates a single [`AtomicU8`] in
//! [`checksums::cpu_features`]. The tests serialise on a shared mutex so
//! that nextest's per-binary thread parallelism does not interleave their
//! mutations. The binary is independent from the `checksums` unit tests, so
//! installing `SimdLevel::None` here cannot trip an unrelated assertion in a
//! parallel unit test.

use std::sync::Mutex;

use checksums::RollingChecksum;
use checksums::SimdLevel;
use checksums::cpu_features::{
    clear_simd_override_for_tests, reset_simd_override_for_tests, simd_override,
};
use checksums::md5_backend::{Backend, Dispatcher};
use checksums::simd_acceleration_available;

static OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

fn with_override<R>(level: SimdLevel, body: impl FnOnce() -> R) -> R {
    let _guard = OVERRIDE_LOCK.lock().expect("override mutex poisoned");
    reset_simd_override_for_tests(level);
    let result = body();
    clear_simd_override_for_tests();
    result
}

#[test]
fn auto_override_does_not_disable_simd_when_available() {
    with_override(SimdLevel::Auto, || {
        let _ = simd_acceleration_available();
        let _ = Dispatcher::detect().backend();
        assert_eq!(simd_override(), SimdLevel::Auto);
    });
}

#[test]
fn none_override_disables_rolling_simd() {
    with_override(SimdLevel::None, || {
        assert!(
            !simd_acceleration_available(),
            "--simd=none must disable rolling-checksum SIMD"
        );
    });
}

#[test]
fn none_override_forces_md5_scalar_backend() {
    with_override(SimdLevel::None, || {
        let dispatcher = Dispatcher::detect();
        assert_eq!(
            dispatcher.backend(),
            Backend::Scalar,
            "--simd=none must force the MD5 scalar dispatcher"
        );
    });
}

#[test]
fn none_override_matches_auto_for_rolling_checksum_value() {
    let mut data = Vec::with_capacity(8 * 1024);
    for i in 0..data.capacity() {
        data.push((i & 0xff) as u8);
    }

    let scalar_value = with_override(SimdLevel::None, || {
        let mut rolling = RollingChecksum::new();
        rolling.update(&data);
        rolling.value()
    });

    let auto_value = with_override(SimdLevel::Auto, || {
        let mut rolling = RollingChecksum::new();
        rolling.update(&data);
        rolling.value()
    });

    assert_eq!(
        scalar_value, auto_value,
        "scalar override must match auto-dispatch byte-for-byte"
    );
}

#[test]
fn override_caps_md5_dispatcher_below_host_capability() {
    with_override(SimdLevel::Sse4, || {
        let backend = Dispatcher::detect().backend();
        assert!(
            !matches!(backend, Backend::Avx512 | Backend::Avx2),
            "Sse4 override must not select AVX-class backends, got {:?}",
            backend,
        );
    });
}

#[test]
fn override_avx2_blocks_avx512() {
    with_override(SimdLevel::Avx2, || {
        let backend = Dispatcher::detect().backend();
        assert_ne!(
            backend,
            Backend::Avx512,
            "Avx2 override must not select AVX-512"
        );
    });
}

#[test]
fn override_neon_blocks_x86_backends() {
    with_override(SimdLevel::Neon, || {
        let backend = Dispatcher::detect().backend();
        assert!(
            !matches!(
                backend,
                Backend::Avx512 | Backend::Avx2 | Backend::Sse41 | Backend::Ssse3 | Backend::Sse2
            ),
            "Neon override must not select x86 backends, got {:?}",
            backend,
        );
    });
}
