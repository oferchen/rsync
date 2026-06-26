//! Linux FICLONE concurrency stress test - drives many parallel
//! `fast_io::try_ficlone` clones on a single CoW filesystem to exercise the
//! shared CoW-support cache (`cow_detect`'s `Mutex<HashMap<fsid, _>>`) and the
//! per-clone fd handling under contention.
//!
//! Like the single-threaded smoke test, it skips gracefully when the test
//! filesystem is not reflink-capable (ext4, tmpfs, overlayfs in containers) by
//! probing one `try_ficlone` up front. A negative probe means the mount can't
//! satisfy the test, not that the wiring is broken, so the test self-skips
//! rather than failing - matching the repository's pre-check-and-degrade rule
//! for tests that depend on external resources.

#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;

use tempfile::tempdir;

fn detect_reflink_support(dir: &std::path::Path) -> bool {
    let probe_src = dir.join(".reflink-probe-src");
    let probe_dst = dir.join(".reflink-probe-dst");
    if let Ok(mut f) = fs::File::create(&probe_src) {
        let _ = f.write_all(b"reflink-probe");
        let _ = f.sync_all();
    } else {
        return false;
    }
    let supported = fast_io::try_ficlone(&probe_src, &probe_dst).is_ok();
    let _ = fs::remove_file(&probe_src);
    let _ = fs::remove_file(&probe_dst);
    supported
}

/// Builds a distinct, recognisable payload for file `i` so a clone that
/// returned another file's data (cross-contamination) is detectable.
fn payload_for(i: usize) -> Vec<u8> {
    let seed = (i as u32).wrapping_mul(2_654_435_761);
    (0..4096u32)
        .map(|j| (seed.wrapping_add(j) & 0xff) as u8)
        .collect()
}

#[test]
fn ficlone_concurrent_clones_are_independent_and_intact() {
    let dir = tempdir().expect("tempdir");
    if !detect_reflink_support(dir.path()) {
        eprintln!(
            "skipping concurrent ficlone stress - {:?} is not a reflink-capable filesystem",
            dir.path()
        );
        return;
    }

    // All sources live on the same filesystem, so every clone hits the same
    // fsid entry in the shared CoW-support cache - the contention point this
    // test is designed to exercise.
    const FILE_COUNT: usize = 64;
    let mut sources = Vec::with_capacity(FILE_COUNT);
    let mut payloads = Vec::with_capacity(FILE_COUNT);
    for i in 0..FILE_COUNT {
        let src = dir.path().join(format!("src-{i:03}.bin"));
        let payload = payload_for(i);
        fs::write(&src, &payload).expect("write source");
        sources.push(src);
        payloads.push(payload);
    }
    let dests: Vec<_> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("dst-{i:03}.bin")))
        .collect();

    // Fan out one clone per file across native threads. A poisoned cache mutex,
    // an fd mix-up, or a torn clone would surface as an Err or a mismatched
    // payload below.
    let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..FILE_COUNT)
            .map(|i| {
                let src = &sources[i];
                let dst = &dests[i];
                scope.spawn(move || {
                    fast_io::try_ficlone(src, dst).map_err(|e| format!("clone {i} failed: {e}"))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("clone thread must not panic"))
            .collect()
    });

    for result in &results {
        assert!(result.is_ok(), "{}", result.as_ref().unwrap_err());
    }

    // Every destination must hold its own source's bytes exactly - no clone may
    // have observed another file's data through the shared cache or fd reuse.
    for i in 0..FILE_COUNT {
        let cloned = fs::read(&dests[i]).expect("read clone");
        assert_eq!(
            cloned, payloads[i],
            "concurrent clone {i} must match its own source byte-for-byte"
        );
    }
}
