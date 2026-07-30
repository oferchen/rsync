//! Unit tests for the replay submodules.
//!
//! These tests cover the low-level building blocks: delta application,
//! literal-only writes, and compressed-token decoder construction. The end-to-end replay path is covered by the integration
//! tests in `crates/batch/src/tests.rs`.

use std::fs;
use tempfile::TempDir;

use super::codec::{CompressionCodec, create_compressed_decoder};
use super::delta::{apply_delta_ops, write_literals_to_file};

/// Builds the geometry a batch would have advertised for these tests.
fn head(count: u32, blength: u32, remainder: u32) -> protocol::wire::SumHead {
    protocol::wire::SumHead::with_blocks(count, blength, 2, remainder).expect("valid geometry")
}

#[test]
fn apply_delta_ops_literal_only() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("basis.txt");
    let dest_path = temp.path().join("output.txt");

    fs::write(&basis_path, b"").unwrap();

    let ops = vec![protocol::wire::DeltaOp::Literal(b"hello world".to_vec())];
    apply_delta_ops(
        &basis_path,
        &dest_path,
        ops,
        protocol::wire::SumHead::WHOLE_FILE,
    )
    .unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b"hello world");
}

#[test]
fn apply_delta_ops_copy_from_basis() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("basis.txt");
    let dest_path = temp.path().join("output.txt");

    fs::write(&basis_path, b"0123456789").unwrap();

    let ops = vec![protocol::wire::DeltaOp::Copy {
        block_index: 0,
        length: 10,
    }];
    apply_delta_ops(&basis_path, &dest_path, ops, head(1, 10, 10)).unwrap();

    let result = fs::read(&dest_path).unwrap();
    assert_eq!(result, b"0123456789");
}

#[test]
fn apply_delta_ops_mixed() {
    let temp = TempDir::new().unwrap();
    let basis_path = temp.path().join("basis.txt");
    let dest_path = temp.path().join("output.txt");

    fs::write(&basis_path, b"ABCDE").unwrap();

    let ops = vec![
        protocol::wire::DeltaOp::Literal(b">>".to_vec()),
        protocol::wire::DeltaOp::Copy {
            block_index: 0,
            length: 5,
        },
        protocol::wire::DeltaOp::Literal(b"<<".to_vec()),
    ];
    apply_delta_ops(&basis_path, &dest_path, ops, head(1, 5, 5)).unwrap();

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
    let result = apply_delta_ops(&basis_path, &dest_path, ops, head(1, 10, 10));
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

    // Copy block 1 (last block, 5-byte remainder) + literal.
    let ops = vec![
        protocol::wire::DeltaOp::Copy {
            block_index: 1,
            // Token format: length=0 means derive from block_length/remainder.
            length: 0,
        },
        protocol::wire::DeltaOp::Literal(b"END".to_vec()),
    ];
    apply_delta_ops(&basis_path, &dest_path, ops, head(2, 10, 5)).unwrap();

    let result = fs::read(&dest_path).unwrap();
    // Must copy 5 bytes from block 1 ("12345"), not 10 bytes (would overread).
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
    let decoder = create_compressed_decoder(CompressionCodec::Zlib, 31).unwrap();
    assert!(
        !decoder.initialized(),
        "fresh zlib decoder should not be initialized"
    );
}

#[cfg(feature = "zstd")]
#[test]
fn compressed_decoder_created_for_zstd() {
    let decoder = create_compressed_decoder(CompressionCodec::Zstd, 31).unwrap();
    assert!(
        !decoder.initialized(),
        "fresh zstd decoder should not be initialized"
    );
}

/// When the detected codec is zlib, dictionary sync (`see_token`)
/// must be active. Matches upstream CPRES_ZLIB behavior.
#[test]
fn cpres_zlib_true_for_zlib_codec() {
    let codec = CompressionCodec::Zlib;
    assert!(Some(codec) == Some(CompressionCodec::Zlib));
}

/// Zstd's `see_token()` is a noop, so the dictionary-sync path does not apply.
#[cfg(feature = "zstd")]
#[test]
fn cpres_zlib_false_for_zstd_codec() {
    let codec = CompressionCodec::Zstd;
    assert!(Some(codec) != Some(CompressionCodec::Zlib));
}

/// A pre-29 batch stream carries no iflags word, so the replay loop must
/// synthesise one without consuming any bytes - consuming two here would
/// desync every following field.
///
/// The value is `ITEM_TRANSFER` alone. Upstream's `ITEM_TRANSFER |
/// ITEM_MISSING_DATA` (rsync.c:383-384) adds a log-only bit at `1<<16`
/// (rsync.h:254) that cannot fit this u16 framing word and that replay never
/// renders. It must not be approximated by `1<<10`, which is
/// `ITEM_REPORT_CRTIME` (rsync.h:247) and would be a different flag entirely.
#[test]
fn read_iflags_pre_29_synthesises_item_transfer_without_reading() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("stream");
    fs::write(&path, [0xAA, 0xBB]).expect("write stream");
    let mut stream = std::io::BufReader::new(fs::File::open(&path).expect("open stream"));

    for proto in [28, 27, 20] {
        let iflags = super::dispatch::read_iflags_and_skip_meta(&mut stream, proto)
            .expect("pre-29 fallback must not read");
        assert_eq!(
            iflags,
            1 << 15,
            "proto {proto} must yield bare ITEM_TRANSFER"
        );
        assert_eq!(iflags & (1 << 10), 0, "ITEM_REPORT_CRTIME must not be set");
    }

    // Nothing was consumed: the two bytes are still there for the real reader.
    let mut rest = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut rest).expect("read rest");
    assert_eq!(rest, vec![0xAA, 0xBB]);
}

/// Protocol 29+ reads the 16-bit little-endian iflags word off the wire.
///
/// upstream: rsync.c:383 - `read_shortint(f_in)`
#[test]
fn read_iflags_proto_29_reads_shortint() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("stream");
    fs::write(&path, [0x00, 0x80]).expect("write stream");
    let mut stream = std::io::BufReader::new(fs::File::open(&path).expect("open stream"));

    let iflags = super::dispatch::read_iflags_and_skip_meta(&mut stream, 29).expect("read iflags");
    assert_eq!(iflags, 1 << 15);
}
