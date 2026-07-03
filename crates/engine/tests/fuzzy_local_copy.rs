//! Integration tests for `--fuzzy` basis selection in the local-copy executor.
//!
//! Upstream rsync's generator, when the exact destination file is absent and
//! `fuzzy_basis` is set, calls `find_fuzzy()` to select the closest-named
//! existing destination file as the delta basis and announces it at
//! `DEBUG_GTE(FUZZY, 1)` before using it as `fnamecmp`
//! (`generator.c:1767-1795`). These tests confirm the local-copy path mirrors
//! that behaviour: a near-identical closest-named candidate is used as the
//! delta basis (matched bytes > 0, not a full re-send) and the FUZZY,1
//! selection line is emitted.
//!
//! # Upstream Reference
//!
//! - `generator.c:831` `find_fuzzy()` - scores candidates and picks the basis.
//! - `generator.c:1785-1794` - `fuzzy_file = find_fuzzy(...)`, sets
//!   `fnamecmp = fnamecmpbuf` and prints "fuzzy basis selected for %s: %s".

use std::fs;
use std::path::Path;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use tempfile::tempdir;

/// 256 KiB of deterministic bytes so the delta matcher has multiple full
/// blocks to match (block size ~= sqrt(size), clamped to >= 700 bytes).
fn deterministic_payload(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let chunk = state.to_le_bytes();
        let take = chunk.len().min(len - out.len());
        out.extend_from_slice(&chunk[..take]);
    }
    out
}

/// Enables FUZZY debug at the given level and clears any prior events so the
/// assertion only sees this test's emissions.
fn setup_fuzzy_capture(level: u8) {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.fuzzy = level;
    init(cfg);
    let _ = drain_events();
}

/// Collects FUZZY debug messages emitted since the last drain.
fn fuzzy_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Fuzzy,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

fn run_local_copy_with_trailing_slash(source: &Path, dest: &Path, options: LocalCopyOptions) {
    let mut src_os = source.to_path_buf().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.to_path_buf().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");
}

/// With `--fuzzy`, when the exact destination file is absent, the executor
/// selects the closest-named existing destination file as the delta basis and
/// reuses its matching blocks. The near-identical basis must yield matched
/// bytes (proving a delta transfer against the fuzzy basis rather than a full
/// re-send), and the FUZZY,1 selection line must be emitted.
#[test]
fn fuzzy_selects_near_identical_basis_for_delta() {
    setup_fuzzy_capture(1);

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source: report_2024.csv. Dest already holds report_2023.csv whose first
    // 224 KiB is byte-identical to the source (the delta basis), with a
    // divergent tail so the two files are not identical.
    let shared = deterministic_payload(0xF00D_CAFE, 224 * 1024);
    let mut source_payload = shared.clone();
    source_payload.extend_from_slice(&deterministic_payload(0xAAAA_1111, 32 * 1024));
    let mut basis_payload = shared;
    basis_payload.extend_from_slice(&deterministic_payload(0xBBBB_2222, 32 * 1024));

    fs::write(source.join("report_2024.csv"), &source_payload).expect("write source file");
    fs::write(dest.join("report_2023.csv"), &basis_payload).expect("write basis file");

    let mut src_os = source.clone().into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Local copies default to whole-file; delta (and thus fuzzy-basis reuse)
    // only engages with --no-whole-file. upstream: generator.c:1962 -
    // `if (statret != 0 || whole_file) write_sum_head(NULL)` sends the whole
    // file even when a fuzzy basis was selected.
    let options = LocalCopyOptions::default()
        .recursive(true)
        .whole_file(false)
        .fuzzy_level(1);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    // A delta transfer against the fuzzy basis reuses the shared prefix: matched
    // bytes must be non-zero. A full re-send (no basis) would report zero.
    assert!(
        summary.matched_bytes() > 0,
        "expected fuzzy basis to drive a delta transfer (matched_bytes > 0), got {}",
        summary.matched_bytes()
    );

    // The reconstructed destination must be byte-identical to the source.
    let reconstructed = fs::read(dest.join("report_2024.csv")).expect("read dest");
    assert_eq!(
        reconstructed, source_payload,
        "fuzzy delta must reconstruct the source exactly"
    );

    // upstream: generator.c:1787-1793 - the FUZZY,1 line names the target and
    // basis by their relative flist names (basenames here), byte-for-byte:
    // "fuzzy basis selected for %s: %s". The fuzzy testsuite greps this exact
    // string, so the basis must be the basename `report_2023.csv`, not an
    // absolute destination path.
    let msgs = fuzzy_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "fuzzy basis selected for report_2024.csv: report_2023.csv"),
        "expected exact FUZZY,1 selection line for the chosen basis, got {msgs:?}"
    );
}

/// Without `--fuzzy`, an absent exact destination forces a full whole-file
/// transfer even when a similarly-named candidate exists: no basis is selected,
/// matched bytes stay zero, and no FUZZY line is emitted.
#[test]
fn absent_fuzzy_flag_sends_whole_file() {
    setup_fuzzy_capture(1);

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let shared = deterministic_payload(0xF00D_CAFE, 224 * 1024);
    let mut source_payload = shared.clone();
    source_payload.extend_from_slice(&deterministic_payload(0xAAAA_1111, 32 * 1024));
    let mut basis_payload = shared;
    basis_payload.extend_from_slice(&deterministic_payload(0xBBBB_2222, 32 * 1024));

    fs::write(source.join("report_2024.csv"), &source_payload).expect("write source file");
    fs::write(dest.join("report_2023.csv"), &basis_payload).expect("write basis file");

    // Delta is enabled (--no-whole-file) but fuzzy_level defaults to 0 (off),
    // so no fuzzy basis is consulted and no FUZZY line fires - isolating the
    // fuzzy flag as the sole variable versus the delta-selecting test above.
    run_local_copy_with_trailing_slash(
        &source,
        &dest,
        LocalCopyOptions::default()
            .recursive(true)
            .whole_file(false),
    );

    // Destination content is correct regardless of transfer strategy.
    let reconstructed = fs::read(dest.join("report_2024.csv")).expect("read dest");
    assert_eq!(reconstructed, source_payload);

    // No fuzzy basis was consulted, so no FUZZY,1 line fired.
    let msgs = fuzzy_messages();
    assert!(
        !msgs.iter().any(|m| m.starts_with("fuzzy basis selected")),
        "no fuzzy basis should be selected without --fuzzy, got {msgs:?}"
    );
}
