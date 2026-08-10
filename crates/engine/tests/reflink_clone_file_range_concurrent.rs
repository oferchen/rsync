//! Linux FICLONERANGE concurrency stress test - drives many parallel
//! `fast_io::try_clone_file_range` range clones on a single CoW filesystem to
//! exercise the delta COPY-token reflink path under contention.
//!
//! This mirrors the whole-file `reflink_ficlone_concurrent` test (REFLINK-3.f)
//! but targets the range ioctl the delta path uses (REFLINK-3/4/5): every
//! basis and destination lives on the same filesystem, so no clone may return
//! `EXDEV`, observe another file's bytes, or tear a partially-cloned range.
//!
//! Like its whole-file sibling, it self-skips when the backing filesystem is
//! not range-reflink-capable (ext4, tmpfs, overlayfs in containers) by probing
//! one aligned `try_clone_file_range` up front. A negative probe means the
//! mount cannot satisfy the test, not that the wiring is broken - matching the
//! repository's pre-check-and-degrade rule for tests that need external
//! resources.

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use tempfile::tempdir;

/// Block-aligned clone unit. FICLONERANGE requires the source offset,
/// destination offset, and length to be multiples of the filesystem block
/// size; 64 KiB is a safe multiple of every CoW block size in use (4/16/64
/// KiB).
const CLONE_LEN: u64 = 64 * 1024;

fn make_basis(path: &Path, payload: &[u8]) -> File {
    let mut f = File::create(path).expect("create basis");
    f.write_all(payload).expect("write basis");
    f.sync_all().ok();
    File::open(path).expect("reopen basis for read")
}

fn make_dest(path: &Path, size: u64) -> File {
    let f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)
        .expect("create dest");
    f.set_len(size).expect("size dest");
    f
}

/// Probes whether the backing filesystem can satisfy an aligned range clone.
fn detect_range_reflink_support(dir: &Path) -> bool {
    let payload = vec![0xC3u8; CLONE_LEN as usize];
    let basis = make_basis(&dir.join(".clonerange-probe-basis"), &payload);
    let dest = make_dest(&dir.join(".clonerange-probe-dest"), CLONE_LEN);
    let ok = matches!(
        fast_io::try_clone_file_range(&basis, 0, &dest, 0, CLONE_LEN),
        Ok(true)
    );
    let _ = std::fs::remove_file(dir.join(".clonerange-probe-basis"));
    let _ = std::fs::remove_file(dir.join(".clonerange-probe-dest"));
    ok
}

/// Distinct, recognisable payload for file `i` so a clone that returned another
/// file's data (cross-contamination) is detectable byte-for-byte.
fn payload_for(i: usize) -> Vec<u8> {
    let seed = (i as u32).wrapping_mul(2_654_435_761);
    (0..CLONE_LEN as u32)
        .map(|j| (seed.wrapping_add(j) & 0xff) as u8)
        .collect()
}

#[test]
fn clone_file_range_concurrent_clones_are_independent_and_intact() {
    let dir = tempdir().expect("tempdir");
    if !detect_range_reflink_support(dir.path()) {
        eprintln!(
            "skipping concurrent FICLONERANGE stress - {:?} is not a range-reflink-capable filesystem",
            dir.path()
        );
        return;
    }

    // All basis + destination files share one filesystem: every range clone is
    // same-fs, so a returned EXDEV would be a real regression, not an
    // environmental skip.
    const FILE_COUNT: usize = 64;
    let mut payloads = Vec::with_capacity(FILE_COUNT);
    let mut basis_paths = Vec::with_capacity(FILE_COUNT);
    let mut dest_paths = Vec::with_capacity(FILE_COUNT);
    for i in 0..FILE_COUNT {
        let payload = payload_for(i);
        let basis_path = dir.path().join(format!("basis-{i:03}.bin"));
        let dest_path = dir.path().join(format!("dest-{i:03}.bin"));
        make_basis(&basis_path, &payload);
        make_dest(&dest_path, CLONE_LEN);
        payloads.push(payload);
        basis_paths.push(basis_path);
        dest_paths.push(dest_path);
    }

    // Fan out one aligned range clone per file across native threads. A torn
    // clone, an fd mix-up, or a cross-filesystem EXDEV would surface as an Err
    // or a mismatched payload below.
    let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..FILE_COUNT)
            .map(|i| {
                let basis_path = &basis_paths[i];
                let dest_path = &dest_paths[i];
                scope.spawn(move || {
                    let basis =
                        File::open(basis_path).map_err(|e| format!("open basis {i}: {e}"))?;
                    let dest = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(dest_path)
                        .map_err(|e| format!("open dest {i}: {e}"))?;
                    match fast_io::try_clone_file_range(&basis, 0, &dest, 0, CLONE_LEN) {
                        Ok(true) => Ok(()),
                        Ok(false) => Err(format!(
                            "range clone {i} unexpectedly declined on a same-fs, aligned request"
                        )),
                        Err(e) => Err(format!("range clone {i} failed: {e}")),
                    }
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

    // Every destination must hold its own basis's bytes exactly - no clone may
    // have observed another file's data or left a partially-written range.
    for i in 0..FILE_COUNT {
        let cloned = std::fs::read(&dest_paths[i]).expect("read clone");
        assert_eq!(
            cloned, payloads[i],
            "concurrent range clone {i} must match its own basis byte-for-byte"
        );
    }
}
