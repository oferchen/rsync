//! Integration tests for `--partial-dir` basis reuse in the local-copy executor.
//!
//! When a prior transfer is interrupted, rsync retains the incomplete file in
//! the `--partial-dir`. On the next run the generator finds that leaf, makes it
//! the delta basis (`fnamecmp`, tagged `FNAMECMP_PARTIAL_DIR`) ahead of both the
//! destination and any fuzzy candidate, and the receiver rewrites the
//! reconstruction into that same leaf in place (`one_inplace`). The retained
//! partial therefore lets the resumed transfer send only the bytes that changed
//! since the interruption instead of re-sending the whole file - the entire
//! point of keeping a partial. These tests pin that the local executor mirrors
//! it: the leaf drives a delta (matched bytes > 0), and with no leaf the same
//! transfer falls back to a full whole-file send (matched bytes == 0), isolating
//! the leaf as the sole cause of the reuse.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/generator.c:2172-2179` - `partialptr = partial_dir_fname(fname)`
//!   when the leaf exists as a regular file.
//! - `rsync-3.5.0/generator.c:2270-2274` `prepare_to_open` - `fnamecmp =
//!   partialptr; fnamecmp_type = FNAMECMP_PARTIAL_DIR`, overriding the
//!   destination and fuzzy basis.
//! - `rsync-3.5.0/receiver.c:1137-1138` - `one_inplace = inplace_partial &&
//!   fnamecmp_type == FNAMECMP_PARTIAL_DIR`, the reconstruction rewrites the leaf.

use std::fs;
use std::path::Path;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use filetime::{FileTime, set_file_mtime};
use tempfile::tempdir;

/// Deterministic bytes so the delta matcher has multiple full blocks to match
/// (block size ~= sqrt(size), clamped to >= 700 bytes).
fn deterministic_payload(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Source content and a near-identical partial leaf: the first 224 KiB is shared
/// (the delta basis), the trailing 32 KiB diverges so a delta must be computed.
fn source_and_leaf() -> (Vec<u8>, Vec<u8>) {
    let shared = deterministic_payload(0xF00D_CAFE, 224 * 1024);
    let mut source = shared.clone();
    source.extend_from_slice(&deterministic_payload(0xAAAA_1111, 32 * 1024));
    let mut leaf = shared;
    leaf.extend_from_slice(&deterministic_payload(0xBBBB_2222, 32 * 1024));
    (source, leaf)
}

fn run(
    source: &Path,
    dest: &Path,
    options: LocalCopyOptions,
) -> engine::local_copy::LocalCopySummary {
    let mut src_os = source.to_path_buf().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.to_path_buf().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds")
}

/// A retained `--partial-dir` leaf is used as the delta basis: the resumed
/// transfer reuses the leaf's shared prefix (matched bytes > 0) instead of
/// re-sending the whole file, reconstructs the source exactly, and consumes the
/// leaf. Reverting the fix (leaf not offered as basis) makes this a whole-file
/// send with `matched_bytes == 0`, so the assertion fails RED.
#[test]
fn partial_dir_leaf_drives_delta_and_reconstructs_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(dest.join(".rsync-partial")).expect("create partial dir");

    let (source_payload, leaf_payload) = source_and_leaf();
    fs::write(source.join("report.csv"), &source_payload).expect("write source");
    // The retained partial from an interrupted run. Backdate it so rsync's
    // quick-check never mistakes the (absent) destination for up-to-date.
    let leaf = dest.join(".rsync-partial/report.csv");
    fs::write(&leaf, &leaf_payload).expect("write leaf");
    set_file_mtime(&leaf, FileTime::from_unix_time(1_577_836_800, 0)).expect("backdate leaf");

    // Local copies default to whole-file; delta (and thus partial-dir reuse)
    // only engages with --no-whole-file.
    let summary = run(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .whole_file(false)
            .with_partial_directory(Some(".rsync-partial")),
    );

    assert!(
        summary.matched_bytes() > 0,
        "retained partial leaf must drive a delta transfer (matched_bytes > 0), got {}",
        summary.matched_bytes()
    );

    let reconstructed = fs::read(dest.join("report.csv")).expect("read dest");
    assert_eq!(
        reconstructed, source_payload,
        "partial-dir delta must reconstruct the source exactly"
    );

    // upstream: receiver.c:1288 finish_transfer renames partialptr onto fname,
    // so the leaf no longer occupies the partial dir after a successful resume.
    assert!(
        !leaf.exists(),
        "successful resume must consume the partial-dir leaf"
    );
}

/// Non-vacuity control: the identical transfer with NO partial-dir leaf has no
/// basis to reuse, so it sends the whole file (`matched_bytes == 0`). This
/// isolates the leaf as the sole cause of the delta in the test above - the
/// options, source, and destination-absent state are otherwise identical.
#[test]
fn absent_partial_dir_leaf_sends_whole_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(dest.join(".rsync-partial")).expect("create partial dir");

    let (source_payload, _leaf_payload) = source_and_leaf();
    fs::write(source.join("report.csv"), &source_payload).expect("write source");

    let summary = run(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .whole_file(false)
            .with_partial_directory(Some(".rsync-partial")),
    );

    assert_eq!(
        summary.matched_bytes(),
        0,
        "with no partial-dir leaf there is no basis, so nothing can match"
    );

    let reconstructed = fs::read(dest.join("report.csv")).expect("read dest");
    assert_eq!(reconstructed, source_payload);
}
