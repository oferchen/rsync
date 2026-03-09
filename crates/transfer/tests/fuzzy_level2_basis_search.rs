//! End-to-end tests for fuzzy level 2 basis file search with renamed files
//! across directories.
//!
//! These tests verify that when `--fuzzy --fuzzy` (`-yy`) is enabled, the
//! receiver's basis file search correctly finds matching files in reference
//! directories (sibling directory search), enabling delta transfers for files
//! that have been moved between directories.
//!
//! # Upstream Reference
//!
//! - `generator.c:1580` - `find_fuzzy_basis()` searches fuzzy_dirlist entries
//! - `options.c:2120` - `fuzzy_basis = basis_dir_cnt + 1` for level 2

use std::num::NonZeroU8;
use std::path::Path;

use tempfile::TempDir;
use transfer::receiver::{BasisFileConfig, find_basis_file_with_config};
use transfer::{ReferenceDirectory, ReferenceDirectoryKind};

/// Generates deterministic content of the specified size.
///
/// Uses a repeating pattern seeded by the given byte so that files with the
/// same seed produce identical content, enabling delta-transfer efficiency
/// verification.
fn generate_content(size: usize, seed: u8) -> Vec<u8> {
    (0..size)
        .map(|i| seed.wrapping_add((i % 251) as u8))
        .collect()
}

/// Verifies that fuzzy level 2 finds a basis file in a reference directory
/// when the exact file does not exist at the destination.
///
/// Scenario: files moved from dir_b to dir_a. The reference directory
/// (simulating a sibling) contains the old copies. Level 2 fuzzy should
/// locate them as basis files for delta transfer.
#[test]
fn fuzzy_level2_finds_basis_in_reference_directory() {
    let temp = TempDir::new().expect("create temp dir");

    // Destination directory - where the file should end up (does not exist yet)
    let dest_base = temp.path().join("dest");
    let dest_dir_a = dest_base.join("dir_a");
    std::fs::create_dir_all(&dest_dir_a).expect("create dest/dir_a");

    // Reference directory - simulates sibling dir with existing copies
    let ref_base = temp.path().join("ref");
    let ref_dir_a = ref_base.join("dir_a");
    std::fs::create_dir_all(&ref_dir_a).expect("create ref/dir_a");

    // Write a 64KB file in the reference directory with a similar name
    let content = generate_content(65_536, 0x42);
    std::fs::write(ref_dir_a.join("data_v1.bin"), &content).expect("write ref file");

    // The file we want to transfer has a similar name
    let file_path = dest_dir_a.join("data_v2.bin");
    let relative_path = Path::new("dir_a/data_v2.bin");

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Compare,
        path: ref_base.clone(),
    }];

    let config = BasisFileConfig {
        file_path: &file_path,
        dest_dir: &dest_dir_a,
        relative_path,
        target_size: content.len() as u64,
        fuzzy_level: 2,
        reference_directories: &ref_dirs,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        checksum_length: NonZeroU8::new(16).unwrap(),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        whole_file: false,
    };

    let result = find_basis_file_with_config(&config);

    assert!(
        !result.is_empty(),
        "Fuzzy level 2 should find basis file in reference directory"
    );
    assert!(
        result.basis_path.is_some(),
        "Basis path should be populated"
    );
    let basis_path = result.basis_path.unwrap();
    assert!(
        basis_path.to_string_lossy().contains("data_v1.bin"),
        "Should have matched the similarly-named file, got: {basis_path:?}"
    );
}

/// Verifies that fuzzy level 1 does NOT search reference directories.
///
/// Same setup as above, but with fuzzy_level=1. The basis file should not
/// be found because level 1 only searches the destination directory.
#[test]
fn fuzzy_level1_does_not_search_reference_directories() {
    let temp = TempDir::new().expect("create temp dir");

    let dest_base = temp.path().join("dest");
    let dest_dir_a = dest_base.join("dir_a");
    std::fs::create_dir_all(&dest_dir_a).expect("create dest/dir_a");

    let ref_base = temp.path().join("ref");
    let ref_dir_a = ref_base.join("dir_a");
    std::fs::create_dir_all(&ref_dir_a).expect("create ref/dir_a");

    let content = generate_content(65_536, 0x42);
    std::fs::write(ref_dir_a.join("data_v1.bin"), &content).expect("write ref file");

    let file_path = dest_dir_a.join("data_v2.bin");
    let relative_path = Path::new("dir_a/data_v2.bin");

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Compare,
        path: ref_base.clone(),
    }];

    let config = BasisFileConfig {
        file_path: &file_path,
        dest_dir: &dest_dir_a,
        relative_path,
        target_size: content.len() as u64,
        fuzzy_level: 1,
        reference_directories: &ref_dirs,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        checksum_length: NonZeroU8::new(16).unwrap(),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        whole_file: false,
    };

    let result = find_basis_file_with_config(&config);

    // Level 1 should not find the file in reference dirs (only dest dir)
    // and dest_dir_a is empty, so no fuzzy match possible
    assert!(
        result.is_empty(),
        "Fuzzy level 1 should not find basis in reference directory"
    );
}

/// Verifies that fuzzy level 2 handles multiple reference directories and
/// picks the best match across all of them.
#[test]
fn fuzzy_level2_selects_best_match_across_reference_dirs() {
    let temp = TempDir::new().expect("create temp dir");

    let dest_base = temp.path().join("dest");
    let dest_dir = dest_base.join("subdir");
    std::fs::create_dir_all(&dest_dir).expect("create dest/subdir");

    // Reference dir 1 - poor match (different extension, different size)
    let ref1_base = temp.path().join("ref1");
    let ref1_subdir = ref1_base.join("subdir");
    std::fs::create_dir_all(&ref1_subdir).expect("create ref1/subdir");
    std::fs::write(ref1_subdir.join("report.dat"), "x".repeat(100)).expect("write ref1 file");

    // Reference dir 2 - good match (same extension, similar name, similar size)
    let ref2_base = temp.path().join("ref2");
    let ref2_subdir = ref2_base.join("subdir");
    std::fs::create_dir_all(&ref2_subdir).expect("create ref2/subdir");
    let content = generate_content(65_536, 0xAB);
    std::fs::write(ref2_subdir.join("report_2023.csv"), &content).expect("write ref2 file");

    let file_path = dest_dir.join("report_2024.csv");
    let relative_path = Path::new("subdir/report_2024.csv");

    let ref_dirs = vec![
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref1_base.clone(),
        },
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: ref2_base.clone(),
        },
    ];

    let config = BasisFileConfig {
        file_path: &file_path,
        dest_dir: &dest_dir,
        relative_path,
        target_size: content.len() as u64,
        fuzzy_level: 2,
        reference_directories: &ref_dirs,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        checksum_length: NonZeroU8::new(16).unwrap(),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        whole_file: false,
    };

    let result = find_basis_file_with_config(&config);

    assert!(
        !result.is_empty(),
        "Should find a basis file across reference directories"
    );
    let basis_path = result.basis_path.unwrap();
    assert!(
        basis_path.to_string_lossy().contains("report_2023.csv"),
        "Should pick the better match (report_2023.csv), got: {basis_path:?}"
    );
}

/// Verifies that fuzzy level 2 generates a valid signature from the basis
/// file, confirming the full pipeline from search through signature
/// generation works end-to-end.
#[test]
fn fuzzy_level2_generates_valid_signature_from_basis() {
    let temp = TempDir::new().expect("create temp dir");

    let dest_base = temp.path().join("dest");
    let dest_dir = dest_base.join("project");
    std::fs::create_dir_all(&dest_dir).expect("create dest/project");

    let ref_base = temp.path().join("ref");
    let ref_project = ref_base.join("project");
    std::fs::create_dir_all(&ref_project).expect("create ref/project");

    // Write a 128KB file so the signature has multiple blocks
    let content = generate_content(131_072, 0xCD);
    std::fs::write(ref_project.join("archive_v1.tar"), &content).expect("write ref file");

    let file_path = dest_dir.join("archive_v2.tar");
    let relative_path = Path::new("project/archive_v2.tar");

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Copy,
        path: ref_base.clone(),
    }];

    let config = BasisFileConfig {
        file_path: &file_path,
        dest_dir: &dest_dir,
        relative_path,
        target_size: content.len() as u64,
        fuzzy_level: 2,
        reference_directories: &ref_dirs,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        checksum_length: NonZeroU8::new(16).unwrap(),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        whole_file: false,
    };

    let result = find_basis_file_with_config(&config);

    assert!(
        !result.is_empty(),
        "Should find basis and generate signature"
    );
    let sig = result.signature.expect("signature should be present");
    assert!(
        !sig.blocks().is_empty(),
        "Signature should contain blocks for a 128KB file"
    );
    assert!(
        sig.blocks().len() > 1,
        "128KB file should produce multiple signature blocks, got {}",
        sig.blocks().len()
    );
}

/// Verifies that fuzzy level 0 (disabled) skips fuzzy search entirely,
/// even when reference directories are configured.
#[test]
fn fuzzy_level0_skips_search() {
    let temp = TempDir::new().expect("create temp dir");

    let dest_base = temp.path().join("dest");
    let dest_dir = dest_base.join("dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest/dir");

    let ref_base = temp.path().join("ref");
    let ref_dir = ref_base.join("dir");
    std::fs::create_dir_all(&ref_dir).expect("create ref/dir");

    let content = generate_content(65_536, 0x11);
    std::fs::write(ref_dir.join("file_old.txt"), &content).expect("write ref file");

    let file_path = dest_dir.join("file_new.txt");
    let relative_path = Path::new("dir/file_new.txt");

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Compare,
        path: ref_base.clone(),
    }];

    let config = BasisFileConfig {
        file_path: &file_path,
        dest_dir: &dest_dir,
        relative_path,
        target_size: content.len() as u64,
        fuzzy_level: 0,
        reference_directories: &ref_dirs,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        checksum_length: NonZeroU8::new(16).unwrap(),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        whole_file: false,
    };

    let result = find_basis_file_with_config(&config);

    assert!(
        result.is_empty(),
        "Fuzzy level 0 should not search for basis files"
    );
}

/// Verifies that whole_file mode bypasses fuzzy search entirely, even at
/// level 2 with matching files available.
#[test]
fn whole_file_bypasses_fuzzy_search() {
    let temp = TempDir::new().expect("create temp dir");

    let dest_base = temp.path().join("dest");
    let dest_dir = dest_base.join("dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest/dir");

    let ref_base = temp.path().join("ref");
    let ref_dir = ref_base.join("dir");
    std::fs::create_dir_all(&ref_dir).expect("create ref/dir");

    let content = generate_content(65_536, 0x22);
    std::fs::write(ref_dir.join("data_v1.bin"), &content).expect("write ref file");

    let file_path = dest_dir.join("data_v2.bin");
    let relative_path = Path::new("dir/data_v2.bin");

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Compare,
        path: ref_base.clone(),
    }];

    let config = BasisFileConfig {
        file_path: &file_path,
        dest_dir: &dest_dir,
        relative_path,
        target_size: content.len() as u64,
        fuzzy_level: 2,
        reference_directories: &ref_dirs,
        protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
        checksum_length: NonZeroU8::new(16).unwrap(),
        checksum_algorithm: signature::SignatureAlgorithm::Md4,
        whole_file: true,
    };

    let result = find_basis_file_with_config(&config);

    assert!(
        result.is_empty(),
        "whole_file mode should bypass all basis file search"
    );
}
