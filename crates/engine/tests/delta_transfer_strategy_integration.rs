//! Integration tests for [`DeltaTransferStrategy`] end-to-end block matching.
//!
//! These tests exercise the strategy with real source and basis files instead
//! of pre-computed stats. They verify that the strategy:
//! - Builds a signature from the basis
//! - Runs [`DeltaGenerator`](engine::DeltaGenerator) over the source
//! - Emits literal/COPY tokens reflecting actual block matches
//! - Applies the script to produce a destination file byte-identical to source
//! - Reports stats derived from the matching outcome
//!
//! All scenarios use [`tempfile::TempDir`] so no environment leaks occur.

use std::fs;
use std::path::Path;

use engine::{DeltaStrategy, DeltaTransferStrategy, DeltaWork};
use tempfile::TempDir;

const ONE_MEBIBYTE: usize = 1024 * 1024;
const SHARED_PREFIX_LEN: usize = 800 * 1024;
const DIVERGENT_TAIL_LEN: usize = 200 * 1024;

/// Generates 1 MiB of deterministic byte content using a simple LCG so adjacent
/// byte windows are unique enough for the rolling+strong checksum matcher.
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

fn write_file(path: &Path, data: &[u8]) {
    fs::write(path, data).expect("write fixture file");
}

fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read output file")
}

/// Scenario 1: 1 MiB source where the first 800 KiB matches the basis and the
/// final 200 KiB diverges. Expects literal_bytes near 200 KiB, matched_bytes
/// near 800 KiB, and a byte-identical destination file.
#[test]
fn delta_transfer_strategy_matches_shared_prefix() {
    let temp = TempDir::new().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let basis_path = temp.path().join("basis.bin");
    let dest_path = temp.path().join("dest.bin");

    let mut source = deterministic_payload(0xCAFE_F00D, ONE_MEBIBYTE);
    assert_eq!(source.len(), ONE_MEBIBYTE);

    let mut basis = source[..SHARED_PREFIX_LEN].to_vec();
    basis.extend_from_slice(&deterministic_payload(0x1234_5678, DIVERGENT_TAIL_LEN));
    assert_eq!(basis.len(), ONE_MEBIBYTE);

    let divergent_tail = deterministic_payload(0xDEAD_BEEF, DIVERGENT_TAIL_LEN);
    source.splice(SHARED_PREFIX_LEN.., divergent_tail.iter().copied());
    assert_eq!(source.len(), ONE_MEBIBYTE);

    write_file(&source_path, &source);
    write_file(&basis_path, &basis);

    let work = DeltaWork::delta_with_source(
        0u32,
        dest_path.clone(),
        basis_path.clone(),
        source_path.clone(),
        source.len() as u64,
    );

    let result = DeltaTransferStrategy::new().process(&work);
    assert!(
        result.is_success(),
        "expected success, got {:?}",
        result.status()
    );

    // Block size for a 1 MiB file is roughly sqrt(1MiB) ~= 1024 bytes (within
    // upstream's 700-16384 clamp). Allow a few-block tolerance on each side
    // since literal padding around the boundary is matcher-dependent.
    let tolerance = 64 * 1024;
    let literal = result.literal_bytes();
    let matched = result.matched_bytes();
    assert!(
        literal >= (DIVERGENT_TAIL_LEN as u64).saturating_sub(tolerance)
            && literal <= DIVERGENT_TAIL_LEN as u64 + tolerance,
        "literal_bytes {literal} outside expected range around {DIVERGENT_TAIL_LEN}"
    );
    assert!(
        matched >= (SHARED_PREFIX_LEN as u64).saturating_sub(tolerance)
            && matched <= SHARED_PREFIX_LEN as u64 + tolerance,
        "matched_bytes {matched} outside expected range around {SHARED_PREFIX_LEN}"
    );
    assert!(matched > 0, "expected at least one matched block");
    assert!(literal > 0, "expected at least one literal byte");

    let reconstructed = read_file(&dest_path);
    assert_eq!(reconstructed, source, "destination must equal source");
    assert_eq!(result.bytes_written(), source.len() as u64);
}

/// Scenario 2: completely disjoint basis - matcher should emit zero matches and
/// the entire source should travel as literal data.
#[test]
fn delta_transfer_strategy_emits_all_literal_when_basis_unrelated() {
    let temp = TempDir::new().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let basis_path = temp.path().join("basis.bin");
    let dest_path = temp.path().join("dest.bin");

    let source = deterministic_payload(0xAAAA_AAAA, ONE_MEBIBYTE);
    let basis = deterministic_payload(0xBBBB_BBBB, ONE_MEBIBYTE);
    assert_ne!(source, basis);

    write_file(&source_path, &source);
    write_file(&basis_path, &basis);

    let work = DeltaWork::delta_with_source(
        1u32,
        dest_path.clone(),
        basis_path.clone(),
        source_path.clone(),
        source.len() as u64,
    );

    let result = DeltaTransferStrategy::new().process(&work);
    assert!(result.is_success(), "{:?}", result.status());
    assert_eq!(result.matched_bytes(), 0, "no blocks should match");
    assert_eq!(result.literal_bytes(), source.len() as u64);
    assert_eq!(result.bytes_written(), source.len() as u64);

    let reconstructed = read_file(&dest_path);
    assert_eq!(reconstructed, source);
}

/// Scenario 3: identical source and basis - matcher should report zero (or at
/// most one block-length worth of) literal bytes for the partial trailing
/// block, with the rest matched.
#[test]
fn delta_transfer_strategy_reports_minimal_literal_when_source_equals_basis() {
    let temp = TempDir::new().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let basis_path = temp.path().join("basis.bin");
    let dest_path = temp.path().join("dest.bin");

    let payload = deterministic_payload(0x9999_9999, ONE_MEBIBYTE);
    write_file(&source_path, &payload);
    write_file(&basis_path, &payload);

    let work = DeltaWork::delta_with_source(
        2u32,
        dest_path.clone(),
        basis_path.clone(),
        source_path.clone(),
        payload.len() as u64,
    );

    let result = DeltaTransferStrategy::new().process(&work);
    assert!(result.is_success(), "{:?}", result.status());

    // Upstream clamps the block size between 700 and 16384 bytes for files of
    // this size. Allow up to 32 KiB literal slack for the trailing partial
    // block plus rolling-window edge effects.
    let max_literal = 32 * 1024u64;
    assert!(
        result.literal_bytes() <= max_literal,
        "literal_bytes {} exceeds {} for identical source/basis",
        result.literal_bytes(),
        max_literal
    );
    assert!(
        result.matched_bytes() >= payload.len() as u64 - max_literal,
        "matched_bytes {} too low for identical source/basis",
        result.matched_bytes()
    );
    assert_eq!(result.bytes_written(), payload.len() as u64);

    let reconstructed = read_file(&dest_path);
    assert_eq!(reconstructed, payload);
}

/// Sanity check: when the work item carries no source path, the strategy
/// preserves its legacy "stats container" behavior so existing pipelines that
/// already split bytes via `receive_data()` stay byte-identical on the wire.
#[test]
fn delta_transfer_strategy_falls_back_to_pre_computed_stats() {
    let work = DeltaWork::delta(
        9u32,
        std::path::PathBuf::from("/dest/unused"),
        std::path::PathBuf::from("/basis/unused"),
        4096,
        1024,
        3072,
    );
    let result = DeltaTransferStrategy::new().process(&work);
    assert!(result.is_success());
    assert_eq!(result.literal_bytes(), 1024);
    assert_eq!(result.matched_bytes(), 3072);
    assert_eq!(result.bytes_written(), 4096);
}
