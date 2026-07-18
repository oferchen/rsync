//! Delta-application surface: whole-file delta replay, wire-to-script
//! conversion, sparse-write state, and `ChecksumVerifier` coverage.

use std::io::{self, Cursor};

use engine::delta::{DeltaScript, DeltaToken};
use protocol::wire::DeltaOp;
use protocol::{ChecksumAlgorithm, NegotiationResult, ProtocolVersion};

use super::support::{apply_whole_file_delta, wire_delta_to_script};
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};

#[test]
fn wire_delta_to_script_converts_literals() {
    let wire_ops = vec![
        DeltaOp::Literal(vec![1, 2, 3, 4]),
        DeltaOp::Literal(vec![5, 6, 7, 8]),
    ];

    let script = wire_delta_to_script(wire_ops);

    assert_eq!(script.tokens().len(), 2);
    assert_eq!(script.total_bytes(), 8);
    assert_eq!(script.literal_bytes(), 8);

    match &script.tokens()[0] {
        DeltaToken::Literal(data) => assert_eq!(data, &vec![1, 2, 3, 4]),
        _ => panic!("expected literal token"),
    }
}

#[test]
fn wire_delta_to_script_converts_copy_operations() {
    let wire_ops = vec![
        DeltaOp::Copy {
            block_index: 0,
            length: 1024,
        },
        DeltaOp::Literal(vec![9, 10]),
        DeltaOp::Copy {
            block_index: 1,
            length: 512,
        },
    ];

    let script = wire_delta_to_script(wire_ops);

    assert_eq!(script.tokens().len(), 3);
    assert_eq!(script.total_bytes(), 1024 + 2 + 512);
    assert_eq!(script.literal_bytes(), 2);

    match &script.tokens()[0] {
        DeltaToken::Copy { index, len } => {
            assert_eq!(*index, 0);
            assert_eq!(*len, 1024);
        }
        _ => panic!("expected copy token"),
    }
}

#[test]
fn apply_whole_file_delta_accepts_only_literals() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("output.txt");

    let tokens = vec![
        DeltaToken::Literal(b"Hello, ".to_vec()),
        DeltaToken::Literal(b"world!".to_vec()),
    ];
    let script = DeltaScript::new(tokens, 13, 13);

    apply_whole_file_delta(&output_path, &script).unwrap();

    let result = std::fs::read(&output_path).unwrap();
    assert_eq!(result, b"Hello, world!");
}

#[test]
fn apply_whole_file_delta_rejects_copy_operations() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("output.txt");

    let tokens = vec![
        DeltaToken::Literal(b"data".to_vec()),
        DeltaToken::Copy {
            index: 0,
            len: 1024,
        },
    ];
    let script = DeltaScript::new(tokens, 1028, 4);

    let result = apply_whole_file_delta(&output_path, &script);

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn apply_whole_file_delta_handles_empty_literals() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("output.txt");

    let script = DeltaScript::new(vec![], 0, 0);

    apply_whole_file_delta(&output_path, &script).unwrap();

    let result = std::fs::read(&output_path).unwrap();
    assert!(result.is_empty());
}

#[test]
fn apply_whole_file_delta_handles_large_literal() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("output.txt");

    let large_data: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
    let tokens = vec![DeltaToken::Literal(large_data.clone())];
    let script = DeltaScript::new(tokens, 65536, 65536);

    apply_whole_file_delta(&output_path, &script).unwrap();

    let result = std::fs::read(&output_path).unwrap();
    assert_eq!(result, large_data);
}

#[test]
fn apply_whole_file_delta_concatenates_multiple_literals() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("output.txt");

    let tokens = vec![
        DeltaToken::Literal(b"part1_".to_vec()),
        DeltaToken::Literal(b"part2_".to_vec()),
        DeltaToken::Literal(b"part3".to_vec()),
    ];
    let script = DeltaScript::new(tokens, 17, 17);

    apply_whole_file_delta(&output_path, &script).unwrap();

    let result = std::fs::read(&output_path).unwrap();
    assert_eq!(result, b"part1_part2_part3");
}

#[test]
fn wire_delta_to_script_handles_empty_input() {
    let wire_ops: Vec<DeltaOp> = vec![];
    let script = wire_delta_to_script(wire_ops);

    assert!(script.is_empty());
    assert_eq!(script.total_bytes(), 0);
    assert_eq!(script.literal_bytes(), 0);
}

#[test]
fn wire_delta_to_script_handles_zero_length_literal() {
    let wire_ops = vec![DeltaOp::Literal(vec![])];
    let script = wire_delta_to_script(wire_ops);

    assert_eq!(script.tokens().len(), 1);
    assert_eq!(script.total_bytes(), 0);
}

#[test]
fn wire_delta_to_script_handles_zero_length_copy() {
    let wire_ops = vec![DeltaOp::Copy {
        block_index: 0,
        length: 0,
    }];
    let script = wire_delta_to_script(wire_ops);

    assert_eq!(script.tokens().len(), 1);
    assert_eq!(script.total_bytes(), 0);
    assert_eq!(script.copy_bytes(), 0);
}

#[test]
fn wire_delta_to_script_mixed_operations() {
    let wire_ops = vec![
        DeltaOp::Copy {
            block_index: 0,
            length: 1024,
        },
        DeltaOp::Literal(vec![0xAB; 128]),
        DeltaOp::Copy {
            block_index: 2,
            length: 512,
        },
        DeltaOp::Literal(vec![0xCD; 64]),
    ];

    let script = wire_delta_to_script(wire_ops);

    assert_eq!(script.tokens().len(), 4);
    assert_eq!(script.total_bytes(), 1024 + 128 + 512 + 64);
    assert_eq!(script.literal_bytes(), 128 + 64);
    assert_eq!(script.copy_bytes(), 1024 + 512);
}

#[test]
fn checksum_verifier_md4_for_legacy_protocol() {
    // Protocol < 30 defaults to MD4
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let mut verifier = ChecksumVerifier::new(None, protocol, 0, None);

    verifier.update(b"test data");
    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    assert_eq!(verifier.finalize_into(&mut buf), 16);
}

#[test]
fn checksum_verifier_md5_for_modern_protocol() {
    // Protocol >= 30 without negotiation defaults to MD5
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut verifier = ChecksumVerifier::new(None, protocol, 12345, None);

    verifier.update(b"test data");
    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    assert_eq!(verifier.finalize_into(&mut buf), 16);
}

#[test]
fn checksum_verifier_xxh3_with_negotiation() {
    use protocol::CompressionAlgorithm;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let negotiated = NegotiationResult {
        checksum: ChecksumAlgorithm::XXH3,
        compression: CompressionAlgorithm::None,
    };

    let mut verifier = ChecksumVerifier::new(Some(&negotiated), protocol, 9999, None);

    verifier.update(b"test data");
    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    // XXH3 produces 8 bytes (64-bit)
    assert_eq!(verifier.finalize_into(&mut buf), 8);
}

#[test]
fn checksum_verifier_sha1_with_negotiation() {
    use protocol::CompressionAlgorithm;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let negotiated = NegotiationResult {
        checksum: ChecksumAlgorithm::SHA1,
        compression: CompressionAlgorithm::None,
    };

    let mut verifier = ChecksumVerifier::new(Some(&negotiated), protocol, 0, None);

    verifier.update(b"test data");
    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    // SHA1 produces 20 bytes
    assert_eq!(verifier.finalize_into(&mut buf), 20);
}

#[test]
fn checksum_verifier_incremental_update() {
    let protocol = ProtocolVersion::try_from(28u8).unwrap();

    let mut verifier1 = ChecksumVerifier::new(None, protocol, 0, None);
    verifier1.update(b"hello ");
    verifier1.update(b"world");
    let mut buf1 = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len1 = verifier1.finalize_into(&mut buf1);

    let mut verifier2 = ChecksumVerifier::new(None, protocol, 0, None);
    verifier2.update(b"hello world");
    let mut buf2 = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len2 = verifier2.finalize_into(&mut buf2);

    assert_eq!(buf1[..len1], buf2[..len2]);
}

#[test]
fn checksum_verifier_empty_data_produces_valid_digest() {
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let verifier = ChecksumVerifier::new(None, protocol, 0, None);

    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    // MD4 produces 16 bytes even for empty input
    assert_eq!(verifier.finalize_into(&mut buf), 16);
}

#[test]
fn checksum_verifier_digest_len_returns_correct_size() {
    use protocol::CompressionAlgorithm;

    // MD4 (protocol < 30)
    let protocol28 = ProtocolVersion::try_from(28u8).unwrap();
    let verifier28 = ChecksumVerifier::new(None, protocol28, 0, None);
    assert_eq!(verifier28.digest_len(), 16);

    // MD5 (protocol >= 30)
    let protocol32 = ProtocolVersion::try_from(32u8).unwrap();
    let verifier32 = ChecksumVerifier::new(None, protocol32, 0, None);
    assert_eq!(verifier32.digest_len(), 16);

    // XXH3 (negotiated)
    let negotiated = NegotiationResult {
        checksum: ChecksumAlgorithm::XXH3,
        compression: CompressionAlgorithm::None,
    };
    let verifier_xxh3 = ChecksumVerifier::new(Some(&negotiated), protocol32, 0, None);
    assert_eq!(verifier_xxh3.digest_len(), 8);

    // SHA1 (negotiated)
    let negotiated_sha1 = NegotiationResult {
        checksum: ChecksumAlgorithm::SHA1,
        compression: CompressionAlgorithm::None,
    };
    let verifier_sha1 = ChecksumVerifier::new(Some(&negotiated_sha1), protocol32, 0, None);
    assert_eq!(verifier_sha1.digest_len(), 20);
}

#[test]
fn sparse_write_state_writes_nonzero_data() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    let data = b"hello world";
    sparse.write(&mut output, data).unwrap();
    sparse.finish(&mut output).unwrap();

    assert_eq!(output.get_ref(), data);
}

#[test]
fn sparse_write_state_skips_zero_runs() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    let zeros = [0u8; 4096];
    let data = b"test";
    sparse.write(&mut output, &zeros).unwrap();
    sparse.write(&mut output, data).unwrap();
    sparse.finish(&mut output).unwrap();

    let result = output.into_inner();
    assert_eq!(result.len(), 4096 + 4);
    assert_eq!(&result[4096..], b"test");
}

#[test]
fn sparse_write_state_handles_trailing_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    let data = b"test";
    let zeros = [0u8; 1024];
    sparse.write(&mut output, data).unwrap();
    sparse.write(&mut output, &zeros).unwrap();
    // finish() returns the logical length and seeks over the trailing hole; the
    // caller establishes the size via set_len (ftruncate) rather than writing a
    // terminal byte. Emulate that here with resize. upstream: fileio.c:43.
    let logical = sparse.finish(&mut output).unwrap();
    output.get_mut().resize(logical as usize, 0);

    let result = output.into_inner();
    assert_eq!(result.len(), 4 + 1024);
    assert_eq!(&result[..4], b"test");
}

#[test]
fn sparse_write_state_mixed_data_and_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    sparse.write(&mut output, b"AAA").unwrap();
    sparse.write(&mut output, &[0u8; 100]).unwrap();
    sparse.write(&mut output, b"BBB").unwrap();
    sparse.finish(&mut output).unwrap();

    let result = output.into_inner();
    assert_eq!(result.len(), 3 + 100 + 3);
    assert_eq!(&result[..3], b"AAA");
    assert_eq!(&result[103..], b"BBB");
}

#[test]
fn sparse_write_state_empty_write() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    let n = sparse.write(&mut output, &[]).unwrap();
    assert_eq!(n, 0);

    sparse.finish(&mut output).unwrap();
    assert!(output.get_ref().is_empty());
}

#[test]
fn sparse_write_state_accumulate_pending_zeros() {
    let mut sparse = SparseWriteState::default();

    sparse.accumulate(100);
    assert_eq!(sparse.pending(), 100);

    sparse.accumulate(50);
    assert_eq!(sparse.pending(), 150);
}

#[test]
fn sparse_write_state_multiple_zero_runs_accumulate() {
    let mut sparse = SparseWriteState::default();

    sparse.accumulate(100);
    sparse.accumulate(200);
    sparse.accumulate(300);

    assert_eq!(sparse.pending(), 600);
}

#[test]
fn sparse_write_state_write_flushes_pending_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    sparse.accumulate(1024);
    sparse.write(&mut output, b"data").unwrap();
    sparse.finish(&mut output).unwrap();

    let result = output.into_inner();
    assert_eq!(result.len(), 1028);
    assert_eq!(&result[1024..], b"data");
}

#[test]
fn sparse_write_state_finish_handles_only_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    sparse.accumulate(4096);
    // finish() returns the logical length; the caller truncates to it (leaving
    // the region a hole that reads back as zeros) instead of materializing a
    // trailing byte. Emulate set_len with resize. upstream: fileio.c:43.
    let logical = sparse.finish(&mut output).unwrap();
    assert_eq!(logical, 4096);
    output.get_mut().resize(logical as usize, 0);

    let result = output.into_inner();
    assert_eq!(result.len(), 4096);
    assert!(result.iter().all(|&b| b == 0));
}
