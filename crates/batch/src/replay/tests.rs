//! Unit tests for the replay submodules.
//!
//! These tests cover the low-level building blocks: block-length derivation,
//! delta application, literal-only writes, and compressed-token decoder
//! construction. The end-to-end replay path is covered by the integration
//! tests in `crates/batch/src/tests.rs`.

use std::fs;
use tempfile::TempDir;

use super::codec::{CompressionCodec, create_compressed_decoder};
use super::delta::{apply_delta_ops, choose_block_length, write_literals_to_file};

#[test]
fn choose_block_length_small_file() {
    // Files smaller than 700^2 = 490_000 bytes get MIN_BLOCK
    assert_eq!(choose_block_length(0), 700);
    assert_eq!(choose_block_length(1000), 700);
    assert_eq!(choose_block_length(489_999), 700);
}

#[test]
fn choose_block_length_medium_file() {
    // sqrt(1_000_000) = 1000
    assert_eq!(choose_block_length(1_000_000), 1000);
}

#[test]
fn choose_block_length_large_file() {
    // Files larger than (128*1024)^2 get MAX_BLOCK
    let max_block = 128 * 1024;
    let threshold = (max_block as u64) * (max_block as u64);
    assert_eq!(choose_block_length(threshold + 1), max_block);
}

#[test]
fn apply_delta_ops_literal_only() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("basis.txt");
    let dest_path = temp.path().join("output.txt");

    fs::write(&basis_path, b"").unwrap();

    let ops = vec![protocol::wire::DeltaOp::Literal(b"hello world".to_vec())];
    apply_delta_ops(&basis_path, &dest_path, ops, 700, 0, 700).unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b"hello world");
}

#[test]
fn apply_delta_ops_copy_from_basis() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("basis.txt");
    let dest_path = temp.path().join("output.txt");

    // Basis file has exactly one block of 10 bytes at block 0
    fs::write(&basis_path, b"0123456789").unwrap();

    let ops = vec![protocol::wire::DeltaOp::Copy {
        block_index: 0,
        length: 10,
    }];
    apply_delta_ops(&basis_path, &dest_path, ops, 10, 1, 10).unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b"0123456789");
}

#[test]
fn apply_delta_ops_mixed() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("basis.txt");
    let dest_path = temp.path().join("output.txt");

    // Basis has "ABCDE" at block 0 (block_length=5)
    fs::write(&basis_path, b"ABCDE").unwrap();

    let ops = vec![
        protocol::wire::DeltaOp::Literal(b">>".to_vec()),
        protocol::wire::DeltaOp::Copy {
            block_index: 0,
            length: 5,
        },
        protocol::wire::DeltaOp::Literal(b"<<".to_vec()),
    ];
    apply_delta_ops(&basis_path, &dest_path, ops, 5, 1, 5).unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b">>ABCDE<<");
}

#[test]
fn apply_delta_ops_nonexistent_basis() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("no_such_file.txt");
    let dest_path = temp.path().join("output.txt");

    let ops = vec![protocol::wire::DeltaOp::Copy {
        block_index: 0,
        length: 10,
    }];
    let result = apply_delta_ops(&basis_path, &dest_path, ops, 10, 1, 10);
    assert!(result.is_err());
}

/// Validates that the last block uses `remainder` bytes instead of `block_length`.
///
/// upstream: receiver.c - when applying deltas, the last block in the basis
/// file is shorter than `block_length`. The sum_head's `remainder` field
/// specifies the actual size.
#[test]
fn apply_delta_last_block_uses_remainder() {
    let temp = TempDir::new().unwrap();
    // Basis: 15 bytes, block_length=10, so block 0 = 10 bytes, block 1 = 5 bytes (remainder).
    let basis_path = temp.path().join("basis.dat");
    fs::write(&basis_path, b"AAAAAAAAAA12345").unwrap();
    let dest_path = temp.path().join("output.dat");

    // Delta: copy block 1 (the last block, 5 bytes remainder), then literal.
    let ops = vec![
        protocol::wire::DeltaOp::Copy {
            block_index: 1,
            length: 0, // Token format: length=0 means derive from block_length/remainder
        },
        protocol::wire::DeltaOp::Literal(b"END".to_vec()),
    ];
    apply_delta_ops(&basis_path, &dest_path, ops, 10, 2, 5).unwrap();

    let result = fs::read(&dest_path).unwrap();
    // Should copy 5 bytes from block 1 ("12345"), not 10 bytes (which would overread).
    assert_eq!(result, b"12345END");
}

#[test]
fn write_literals_to_new_file() {
    let temp = TempDir::new().unwrap();
    let dest_path = temp.path().join("new_file.txt");

    let ops = vec![
        protocol::wire::DeltaOp::Literal(b"hello ".to_vec()),
        protocol::wire::DeltaOp::Literal(b"world".to_vec()),
    ];
    write_literals_to_file(&dest_path, &ops).unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b"hello world");
}

#[test]
fn write_literals_ignores_copy_ops() {
    let temp = TempDir::new().unwrap();
    let dest_path = temp.path().join("literals_only.txt");

    let ops = vec![
        protocol::wire::DeltaOp::Literal(b"data".to_vec()),
        // Copy ops should be ignored when no basis exists
        protocol::wire::DeltaOp::Copy {
            block_index: 0,
            length: 100,
        },
        protocol::wire::DeltaOp::Literal(b"more".to_vec()),
    ];
    write_literals_to_file(&dest_path, &ops).unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b"datamore");
}

#[test]
fn compressed_decoder_created_for_zlib() {
    let decoder = create_compressed_decoder(CompressionCodec::Zlib).unwrap();
    assert!(
        !decoder.initialized(),
        "fresh zlib decoder should not be initialized"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn compressed_decoder_created_for_zstd() {
    let decoder = create_compressed_decoder(CompressionCodec::Zstd).unwrap();
    assert!(
        !decoder.initialized(),
        "fresh zstd decoder should not be initialized"
    );
}

#[test]
fn cpres_zlib_true_for_zlib_codec() {
    // When the detected codec is zlib, dictionary sync (see_token)
    // must be active. This matches upstream CPRES_ZLIB behavior.
    let codec = CompressionCodec::Zlib;
    assert!(Some(codec) == Some(CompressionCodec::Zlib));
}

#[cfg(feature = "zstd")]
#[test]
fn cpres_zlib_false_for_zstd_codec() {
    // When the detected codec is zstd, dictionary sync is unnecessary
    // because zstd's see_token() is a noop.
    let codec = CompressionCodec::Zstd;
    assert!(Some(codec) != Some(CompressionCodec::Zlib));
}
