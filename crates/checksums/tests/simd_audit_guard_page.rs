//! Guard-page over-read harness for the SIMD rolling checksum.
//!
//! Places the input buffer flush against an unreadable page so any load past
//! `buf + len` faults here. Mirrors upstream 3.5.0's `test_no_overread()` in
//! `simd-checksum-x86_64.cpp`.

#![cfg(unix)]

use checksums::RollingChecksum;
use checksums::cpu_features::{SimdLevel, reset_simd_override_for_tests};

fn checksum_at(level: SimdLevel, data: &[u8]) -> u32 {
    reset_simd_override_for_tests(level);
    let mut c = RollingChecksum::new();
    c.update(data);
    c.value()
}

/// Reports which backends this host can actually exercise. A pass on a host
/// without the vector unit proves nothing about that unit, and must not read
/// as coverage.
fn report_coverage() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    println!(
        "guard-page: avx2={} sse2={}",
        std::arch::is_x86_feature_detected!("avx2"),
        std::arch::is_x86_feature_detected!("sse2")
    );
    #[cfg(target_arch = "aarch64")]
    println!(
        "guard-page: neon={}",
        std::arch::is_aarch64_feature_detected!("neon")
    );
    println!(
        "guard-page: simd_acceleration_available={}",
        checksums::simd_acceleration_available()
    );
}

#[test]
fn simd_rolling_checksum_never_reads_past_the_slice_end() {
    report_coverage();
    let pagesz = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(pagesz > 0, "sysconf(_SC_PAGESIZE) gave {pagesz}");
    let pagesz = pagesz as usize;
    let region = pagesz * 4;

    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            region,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(base, libc::MAP_FAILED, "mmap failed");
    let base = base as *mut u8;

    let rc =
        unsafe { libc::mprotect(base.add(region - pagesz) as *mut _, pagesz, libc::PROT_NONE) };
    assert_eq!(rc, 0, "mprotect failed");

    let levels: &[SimdLevel] = &[
        SimdLevel::Auto,
        SimdLevel::Avx2,
        SimdLevel::Sse4,
        SimdLevel::Neon,
        SimdLevel::None,
    ];

    // Every length up to 3 pages, so every remainder mod 16/32/64 and both
    // alignments are covered with the last byte abutting the guard page.
    for len in 1..=4096usize {
        let buf = unsafe { base.add(region - pagesz - len) };
        for i in 0..len {
            unsafe { buf.add(i).write(((i + (i % 3) + (i % 11)) % 256) as u8) };
        }
        let data = unsafe { std::slice::from_raw_parts(buf, len) };

        let reference = checksum_at(SimdLevel::None, data);
        for &level in levels {
            assert_eq!(
                checksum_at(level, data),
                reference,
                "len={len} level={}",
                level.as_cli_str()
            );
        }
    }

    unsafe { libc::munmap(base as *mut _, region) };
}
