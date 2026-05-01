#[cfg(feature = "incremental-flist")]
use super::PHASE1_CHECKSUM_LENGTH;
use super::directory::FailedDirectories;
use super::stats::TransferStats;
use super::wire::SenderAttrs;
use super::wire::SumHead;
use super::{REDO_CHECKSUM_LENGTH, ReceiverContext};
use crate::config::ServerConfig;
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::error::{
    DeltaFatalError, DeltaRecoverableError, DeltaTransferError, categorize_io_error,
};
use crate::flags::ParsedServerFlags;
use crate::handshake::HandshakeResult;
use crate::pipeline::PipelineConfig;
use crate::role::ServerRole;
use crate::temp_guard::TempFileGuard;

use engine::delta::{DeltaScript, DeltaToken};
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;
use protocol::wire::DeltaOp;
use protocol::{ChecksumAlgorithm, NegotiationResult, ProtocolVersion};

use std::ffi::OsString;
use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;

fn test_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

fn test_handshake() -> HandshakeResult {
    test_handshake_with_protocol(32)
}

/// Creates a [`HandshakeResult`] with a specific protocol version for testing.
fn test_handshake_with_protocol(protocol_version: u8) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Applies a delta script to create a new file (whole-file transfer, no basis).
///
/// All tokens must be Literal; Copy operations indicate a protocol error.
fn apply_whole_file_delta(path: &std::path::Path, script: &DeltaScript) -> io::Result<()> {
    let mut output = std::fs::File::create(path)?;

    for token in script.tokens() {
        match token {
            DeltaToken::Literal(data) => {
                output.write_all(data)?;
            }
            DeltaToken::Copy { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Copy operation in whole-file transfer (no basis exists)",
                ));
            }
        }
    }

    output.sync_all()?;
    Ok(())
}

/// Converts wire protocol delta operations to engine delta script.
fn wire_delta_to_script(ops: Vec<DeltaOp>) -> DeltaScript {
    let mut tokens = Vec::with_capacity(ops.len());
    let mut total_bytes = 0u64;
    let mut literal_bytes = 0u64;

    for op in ops {
        match op {
            DeltaOp::Literal(data) => {
                let len = data.len() as u64;
                total_bytes += len;
                literal_bytes += len;
                tokens.push(DeltaToken::Literal(data));
            }
            DeltaOp::Copy {
                block_index,
                length,
            } => {
                total_bytes += length as u64;
                tokens.push(DeltaToken::Copy {
                    index: block_index as u64,
                    len: length as usize,
                });
            }
        }
    }

    DeltaScript::new(tokens, total_bytes, literal_bytes)
}

#[test]
fn receiver_context_creation() {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    assert_eq!(ctx.protocol().as_u8(), 32);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn receiver_empty_file_list() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty file list (just the end marker)
    let data = [0u8];
    let mut cursor = Cursor::new(&data[..]);

    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn receiver_single_file() {
    use protocol::flist::{FileEntry, FileListWriter};

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Create a proper file list using FileListWriter for protocol 32
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(handshake.protocol);

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &entry).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let count = ctx.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 1);
    assert_eq!(ctx.file_list().len(), 1);
    assert_eq!(ctx.file_list()[0].name(), "test.txt");
}

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

    // Create a delta script with only literals
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

    // Create a delta script with a copy operation (invalid for whole-file transfer)
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
fn temp_file_guard_cleans_up_on_drop() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().join("test.tmp");

    // Create temp file
    std::fs::write(&temp_path, b"test data").unwrap();
    assert!(temp_path.exists());

    {
        let _guard = TempFileGuard::new(temp_path.clone());
        // Guard goes out of scope here, should delete file
    }

    // File should be deleted
    assert!(!temp_path.exists());
}

#[test]
fn temp_file_guard_keeps_file_when_marked() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path().join("test.tmp");

    // Create temp file
    std::fs::write(&temp_path, b"test data").unwrap();
    assert!(temp_path.exists());

    {
        let mut guard = TempFileGuard::new(temp_path.clone());
        guard.keep(); // Mark as successful
        // Guard goes out of scope here
    }

    // File should still exist
    assert!(temp_path.exists());
}

#[test]
fn error_categorization_disk_full_is_fatal() {
    use std::path::Path;

    let err = io::Error::from(io::ErrorKind::StorageFull);
    let path = Path::new("/tmp/test.txt");

    let categorized = categorize_io_error(err, path, "write");

    match categorized {
        DeltaTransferError::Fatal(DeltaFatalError::DiskFull { path: p, .. }) => {
            assert_eq!(p, path);
        }
        _ => panic!("Expected fatal disk full error"),
    }
}

#[test]
fn error_categorization_permission_denied_is_recoverable() {
    use std::path::Path;

    let err = io::Error::from(io::ErrorKind::PermissionDenied);
    let path = Path::new("/tmp/test.txt");

    let categorized = categorize_io_error(err, path, "open");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
            path: p,
            operation: op,
        }) => {
            assert_eq!(p, path);
            assert_eq!(op, "open");
        }
        _ => panic!("Expected recoverable permission denied error"),
    }
}

#[test]
fn error_categorization_not_found_is_recoverable() {
    use std::path::Path;

    let err = io::Error::from(io::ErrorKind::NotFound);
    let path = Path::new("/tmp/test.txt");

    let categorized = categorize_io_error(err, path, "open");

    match categorized {
        DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound { path: p }) => {
            assert_eq!(p, path);
        }
        _ => panic!("Expected recoverable file not found error"),
    }
}

#[test]
fn transfer_stats_tracks_metadata_errors() {
    let mut stats = TransferStats::default();

    assert_eq!(stats.metadata_errors.len(), 0);

    // Simulate collecting metadata errors
    stats.metadata_errors.push((
        PathBuf::from("/tmp/file1.txt"),
        "Permission denied".to_owned(),
    ));
    stats.metadata_errors.push((
        PathBuf::from("/tmp/file2.txt"),
        "Operation not permitted".to_owned(),
    ));

    assert_eq!(stats.metadata_errors.len(), 2);
    assert_eq!(stats.metadata_errors[0].0, PathBuf::from("/tmp/file1.txt"));
    assert_eq!(stats.metadata_errors[0].1, "Permission denied");
}

#[test]
fn checksum_verifier_md4_for_legacy_protocol() {
    // Protocol < 30 defaults to MD4
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let mut verifier = ChecksumVerifier::new(None, protocol, 0, None);

    verifier.update(b"test data");
    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    // MD4 produces 16 bytes
    assert_eq!(verifier.finalize_into(&mut buf), 16);
}

#[test]
fn checksum_verifier_md5_for_modern_protocol() {
    // Protocol >= 30 without negotiation defaults to MD5
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut verifier = ChecksumVerifier::new(None, protocol, 12345, None);

    verifier.update(b"test data");
    let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

    // MD5 produces 16 bytes
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
    // Test that incremental updates produce same result as single update
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
fn sparse_write_state_writes_nonzero_data() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    // Write non-zero data
    let data = b"hello world";
    sparse.write(&mut output, data).unwrap();
    sparse.finish(&mut output).unwrap();

    // Should write the data directly
    assert_eq!(output.get_ref(), data);
}

#[test]
fn sparse_write_state_skips_zero_runs() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    // Write zeros followed by data
    let zeros = [0u8; 4096];
    let data = b"test";
    sparse.write(&mut output, &zeros).unwrap();
    sparse.write(&mut output, data).unwrap();
    sparse.finish(&mut output).unwrap();

    // Output should be mostly zeros (sparse seek) followed by "test"
    // The file position should be at zeros.len() + data.len()
    let result = output.into_inner();
    assert_eq!(result.len(), 4096 + 4);
    // Last 4 bytes should be "test"
    assert_eq!(&result[4096..], b"test");
}

#[test]
fn sparse_write_state_handles_trailing_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    // Write data followed by zeros
    let data = b"test";
    let zeros = [0u8; 1024];
    sparse.write(&mut output, data).unwrap();
    sparse.write(&mut output, &zeros).unwrap();
    sparse.finish(&mut output).unwrap();

    // File should be extended to correct size
    let result = output.into_inner();
    assert_eq!(result.len(), 4 + 1024);
    assert_eq!(&result[..4], b"test");
}

#[test]
fn sparse_write_state_mixed_data_and_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    // Interleaved data and zeros
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

    // Empty write should be a no-op
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
fn sum_head_new_creates_with_correct_values() {
    let sum_head = SumHead::new(100, 1024, 16, 512);
    assert_eq!(sum_head.count, 100);
    assert_eq!(sum_head.blength, 1024);
    assert_eq!(sum_head.s2length, 16);
    assert_eq!(sum_head.remainder, 512);
}

#[test]
fn sum_head_empty_creates_zero_values() {
    let sum_head = SumHead::empty();
    assert_eq!(sum_head.count, 0);
    assert_eq!(sum_head.blength, 0);
    assert_eq!(sum_head.s2length, 0);
    assert_eq!(sum_head.remainder, 0);
    assert!(sum_head.is_empty());
}

#[test]
fn sum_head_default_is_empty() {
    let sum_head = SumHead::default();
    assert!(sum_head.is_empty());
    assert_eq!(sum_head, SumHead::empty());
}

#[test]
fn sum_head_is_empty_false_for_nonzero_count() {
    let sum_head = SumHead::new(1, 1024, 16, 0);
    assert!(!sum_head.is_empty());
}

#[test]
fn sum_head_write_produces_correct_wire_format() {
    let sum_head = SumHead::new(10, 700, 16, 100);
    let mut output = Vec::new();
    sum_head.write(&mut output).unwrap();

    assert_eq!(output.len(), 16);
    // All values as 32-bit little-endian
    assert_eq!(
        i32::from_le_bytes([output[0], output[1], output[2], output[3]]),
        10
    );
    assert_eq!(
        i32::from_le_bytes([output[4], output[5], output[6], output[7]]),
        700
    );
    assert_eq!(
        i32::from_le_bytes([output[8], output[9], output[10], output[11]]),
        16
    );
    assert_eq!(
        i32::from_le_bytes([output[12], output[13], output[14], output[15]]),
        100
    );
}

#[test]
fn sum_head_read_parses_wire_format() {
    // Prepare wire data: count=5, blength=512, s2length=16, remainder=128
    let mut data = Vec::new();
    data.extend_from_slice(&5i32.to_le_bytes());
    data.extend_from_slice(&512i32.to_le_bytes());
    data.extend_from_slice(&16i32.to_le_bytes());
    data.extend_from_slice(&128i32.to_le_bytes());

    let sum_head = SumHead::read(&mut Cursor::new(data)).unwrap();

    assert_eq!(sum_head.count, 5);
    assert_eq!(sum_head.blength, 512);
    assert_eq!(sum_head.s2length, 16);
    assert_eq!(sum_head.remainder, 128);
}

#[test]
fn sum_head_round_trip() {
    let original = SumHead::new(100, 1024, 20, 256);

    let mut buf = Vec::new();
    original.write(&mut buf).unwrap();

    let decoded = SumHead::read(&mut Cursor::new(buf)).unwrap();
    assert_eq!(original, decoded);
}

#[test]
fn sum_head_read_insufficient_data() {
    // Only 8 bytes instead of 16
    let data = vec![0u8; 8];
    let result = SumHead::read(&mut Cursor::new(data));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sender_attrs_read_protocol_28_returns_default_iflags() {
    // Protocol 28 just reads the NDX byte, no iflags
    let data = vec![0x05u8]; // NDX byte only
    let attrs = SenderAttrs::read(&mut Cursor::new(data), 28).unwrap();

    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
    assert!(attrs.fnamecmp_type.is_none());
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_protocol_29_parses_iflags() {
    // NDX byte + iflags (0x8000 = ITEM_TRANSFER)
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x8000u16.to_le_bytes()); // iflags

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x8000);
    assert!(attrs.fnamecmp_type.is_none());
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_with_basis_type() {
    // NDX byte + iflags (0x8800 = ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS) + fnamecmp_type
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x8800u16.to_le_bytes()); // iflags with BASIS_TYPE_FOLLOWS
    data.push(0x02); // fnamecmp_type = BasisDir(2)

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x8800);
    assert_eq!(
        attrs.fnamecmp_type,
        Some(protocol::FnameCmpType::BasisDir(2))
    );
    assert!(attrs.xname.is_none());
}

#[test]
fn sender_attrs_read_with_short_xname() {
    // NDX byte + iflags (0x9000 = ITEM_TRANSFER | ITEM_XNAME_FOLLOWS) + xname
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
    data.push(0x04); // xname length (short form)
    data.extend_from_slice(b"test"); // xname content

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x9000);
    assert!(attrs.fnamecmp_type.is_none());
    assert_eq!(attrs.xname, Some(b"test".to_vec()));
}

#[test]
fn sender_attrs_read_with_long_xname() {
    // NDX + iflags + xname with extended length (> 127 bytes requires 2-byte length)
    let mut data = vec![0x05u8]; // NDX byte
    data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
    // Length 300 = 0x80 | (300 / 256) = 0x81, then 300 % 256 = 44
    data.push(0x81); // High byte: 0x80 flag + 1
    data.push(0x2C); // Low byte: 44 (1*256 + 44 = 300)
    data.extend(vec![b'x'; 300]); // xname content (300 'x' characters)

    let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

    assert_eq!(attrs.iflags, 0x9000);
    assert!(attrs.fnamecmp_type.is_none());
    assert_eq!(attrs.xname.as_ref().unwrap().len(), 300);
}

#[test]
fn sender_attrs_read_empty_returns_eof_error() {
    let data: Vec<u8> = vec![];
    let result = SenderAttrs::read(&mut Cursor::new(data), 29);

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sender_attrs_constants_match_upstream() {
    // Verify our constants match upstream rsync.h values
    assert_eq!(SenderAttrs::ITEM_TRANSFER, 0x8000);
    assert_eq!(SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS, 0x0800);
    assert_eq!(SenderAttrs::ITEM_XNAME_FOLLOWS, 0x1000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_30_delta_encoded() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Simulate sender encoding NDX 0 for protocol 30+
    // With prev_positive=-1, ndx=0, diff=1, encoded as single byte 0x01
    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 0).unwrap();
    // Add iflags (ITEM_TRANSFER = 0x8000)
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    // Receiver reads with its own codec
    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 0);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_30_sequential_indices() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Simulate sender sending sequential indices 0, 1, 2
    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    for ndx in 0..3 {
        sender_codec.write_ndx(&mut wire_data, ndx).unwrap();
        wire_data.extend_from_slice(&0x8000u16.to_le_bytes());
    }

    // Receiver reads all three
    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);

    for expected_ndx in 0..3 {
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();
        assert_eq!(ndx, expected_ndx, "expected NDX {expected_ndx}");
        assert_eq!(attrs.iflags, 0x8000);
    }
}

#[test]
fn sender_attrs_read_with_codec_legacy_protocol_29() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Protocol 29 uses 4-byte LE NDX
    let mut sender_codec = create_ndx_codec(29);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 42).unwrap();
    // Add iflags
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    let mut receiver_codec = create_ndx_codec(29);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 42);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn sender_attrs_read_with_codec_protocol_28_no_iflags() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Protocol 28: 4-byte LE NDX, no iflags
    let mut sender_codec = create_ndx_codec(28);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, 5).unwrap();
    // No iflags for protocol < 29

    let mut receiver_codec = create_ndx_codec(28);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, 5);
    // Default iflags for protocol < 29
    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
}

#[test]
fn sender_attrs_read_with_codec_large_index() {
    use protocol::codec::{NdxCodec, create_ndx_codec};

    // Test with a large index that requires extended encoding in protocol 30+
    let large_index = 50000;

    let mut sender_codec = create_ndx_codec(31);
    let mut wire_data = Vec::new();
    sender_codec.write_ndx(&mut wire_data, large_index).unwrap();
    wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

    let mut receiver_codec = create_ndx_codec(31);
    let mut cursor = Cursor::new(&wire_data);
    let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

    assert_eq!(ndx, large_index);
    assert_eq!(attrs.iflags, 0x8000);
}

#[test]
fn basis_file_result_is_empty_when_no_signature() {
    use super::basis::BasisFileResult;

    let result = BasisFileResult {
        signature: None,
        basis_path: None,
    };
    assert!(result.is_empty());
}

#[test]
fn basis_file_result_is_not_empty_when_has_signature() {
    use super::basis::BasisFileResult;
    use engine::delta::SignatureLayout;
    use engine::signature::FileSignature;
    use std::num::NonZeroU32;

    // Create a minimal signature
    let layout =
        SignatureLayout::from_raw_parts(NonZeroU32::new(512).unwrap(), 0, 0, REDO_CHECKSUM_LENGTH);
    let signature = FileSignature::from_raw_parts(layout, vec![], 0);

    let result = BasisFileResult {
        signature: Some(signature),
        basis_path: Some(PathBuf::from("/tmp/basis")),
    };
    assert!(!result.is_empty());
}

#[test]
fn try_reference_directories_finds_file_in_first_directory() {
    use super::basis::try_reference_directories;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};
    // Create two reference directories
    let ref_dir1 = test_support::create_tempdir();
    let ref_dir2 = test_support::create_tempdir();

    // Create a file in the first reference directory
    let test_file = ref_dir1.path().join("subdir/test.txt");
    std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
    std::fs::write(&test_file, b"test content from ref1").unwrap();

    let ref_dirs = vec![
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref_dir1.path().to_path_buf(),
        },
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: ref_dir2.path().to_path_buf(),
        },
    ];

    let relative_path = std::path::Path::new("subdir/test.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_some());
    let (_, size, path) = result.unwrap();
    assert_eq!(size, 22); // "test content from ref1".len()
    assert_eq!(path, test_file);
}

#[test]
fn try_reference_directories_finds_file_in_second_directory() {
    use super::basis::try_reference_directories;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};
    // Create two reference directories
    let ref_dir1 = test_support::create_tempdir();
    let ref_dir2 = test_support::create_tempdir();

    // Create a file only in the second reference directory
    let test_file = ref_dir2.path().join("test.txt");
    std::fs::write(&test_file, b"test content from ref2").unwrap();

    let ref_dirs = vec![
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref_dir1.path().to_path_buf(),
        },
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Copy,
            path: ref_dir2.path().to_path_buf(),
        },
    ];

    let relative_path = std::path::Path::new("test.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_some());
    let (_, size, path) = result.unwrap();
    assert_eq!(size, 22); // "test content from ref2".len()
    assert_eq!(path, test_file);
}

#[test]
fn try_reference_directories_returns_none_when_not_found() {
    use super::basis::try_reference_directories;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};
    let ref_dir = test_support::create_tempdir();

    let ref_dirs = vec![ReferenceDirectory {
        kind: ReferenceDirectoryKind::Link,
        path: ref_dir.path().to_path_buf(),
    }];

    let relative_path = std::path::Path::new("nonexistent.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_none());
}

#[test]
fn try_reference_directories_empty_list_returns_none() {
    use super::basis::try_reference_directories;
    use crate::config::ReferenceDirectory;

    let ref_dirs: Vec<ReferenceDirectory> = vec![];
    let relative_path = std::path::Path::new("test.txt");
    let result = try_reference_directories(relative_path, &ref_dirs);

    assert!(result.is_none());
}

/// Creates test config with specific flags for ID list tests.
fn config_with_flags(owner: bool, group: bool, numeric_ids: bool) -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            owner,
            group,
            numeric_ids,
            ..ParsedServerFlags::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

#[test]
fn receive_id_lists_skips_when_numeric_ids_true() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, true);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // With numeric_ids=true, no data should be read even with owner/group set
    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    // Cursor position unchanged - nothing read
    assert_eq!(cursor.position(), 0);
}

#[test]
fn receive_id_lists_reads_uid_list_when_owner_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, false, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty UID list: varint 0 terminator only
    let data: &[u8] = &[0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 1);
}

#[test]
fn receive_id_lists_reads_gid_list_when_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, true, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty GID list: varint 0 terminator only
    let data: &[u8] = &[0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 1);
}

#[test]
fn receive_id_lists_reads_both_when_owner_and_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Both lists: two varint 0 terminators
    let data: &[u8] = &[0, 0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 2);
}

#[test]
fn receive_id_lists_skips_both_when_neither_flag_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, false, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 0);
}

#[test]
fn apply_whole_file_delta_handles_empty_literals() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("output.txt");

    // Empty delta script (no tokens)
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

    // Large literal (64KB)
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

    // Multiple small literals
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
    // Simulate typical rsync delta: copy unchanged block, insert literal, copy another block
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
fn checksum_verifier_empty_data_produces_valid_digest() {
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let verifier = ChecksumVerifier::new(None, protocol, 0, None);

    // No updates, just finalize
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
fn sparse_write_state_multiple_zero_runs_accumulate() {
    let mut sparse = SparseWriteState::default();

    // Accumulate multiple zero runs
    sparse.accumulate(100);
    sparse.accumulate(200);
    sparse.accumulate(300);

    assert_eq!(sparse.pending(), 600);
}

#[test]
fn sparse_write_state_write_flushes_pending_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    // Accumulate zeros then write data
    sparse.accumulate(1024);
    sparse.write(&mut output, b"data").unwrap();
    sparse.finish(&mut output).unwrap();

    let result = output.into_inner();
    // File should be 1024 zeros + "data"
    assert_eq!(result.len(), 1028);
    assert_eq!(&result[1024..], b"data");
}

#[test]
fn sparse_write_state_finish_handles_only_zeros() {
    let mut output = Cursor::new(Vec::new());
    let mut sparse = SparseWriteState::default();

    // Only zeros, no data
    sparse.accumulate(4096);
    sparse.finish(&mut output).unwrap();

    let result = output.into_inner();
    // File should extend to 4096 bytes of zeros
    assert_eq!(result.len(), 4096);
    assert!(result.iter().all(|&b| b == 0));
}

#[test]
fn incremental_receiver_reads_entries() {
    // Create test data with a simple file list
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add a directory and a file
    let dir = FileEntry::new_directory("testdir".into(), 0o755);
    let file = FileEntry::new_file("testdir/file.txt".into(), 100, 0o644);

    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    // Create handshake and config
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    // Create incremental receiver
    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // First entry should be the directory (it has no parent dependency)
    let entry1 = receiver.next_ready().unwrap().unwrap();
    assert!(entry1.is_dir());
    assert_eq!(entry1.name(), "testdir");

    // Second entry should be the file (parent dir now exists)
    let entry2 = receiver.next_ready().unwrap().unwrap();
    assert!(entry2.is_file());
    assert_eq!(entry2.name(), "testdir/file.txt");

    // No more entries
    assert!(receiver.next_ready().unwrap().is_none());
    assert!(receiver.is_empty());
    assert_eq!(receiver.entries_read(), 2);
}

#[test]
fn incremental_receiver_handles_empty_list() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let writer = protocol::flist::FileListWriter::new(protocol);
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    assert!(receiver.next_ready().unwrap().is_none());
    assert!(receiver.is_empty());
    assert_eq!(receiver.entries_read(), 0);
}

#[test]
fn incremental_receiver_collect_sorted() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add entries in random order
    let file1 = FileEntry::new_file("z_file.txt".into(), 50, 0o644);
    let file2 = FileEntry::new_file("a_file.txt".into(), 100, 0o644);
    let dir = FileEntry::new_directory("m_dir".into(), 0o755);

    writer.write_entry(&mut data, &file1).unwrap();
    writer.write_entry(&mut data, &file2).unwrap();
    writer.write_entry(&mut data, &dir).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // collect_sorted should return entries in sorted order
    let entries = receiver.collect_sorted().unwrap();
    assert_eq!(entries.len(), 3);

    // Files should come before directories at the same level
    assert_eq!(entries[0].name(), "a_file.txt");
    assert_eq!(entries[1].name(), "z_file.txt");
    assert_eq!(entries[2].name(), "m_dir");
}

#[test]
fn incremental_receiver_iterator_interface() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    let file = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // Use iterator interface
    let entries: Vec<_> = receiver.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "test.txt");
}

#[test]
fn incremental_receiver_mark_directory_created() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    // Add only a nested file (no directory entry)
    let file = FileEntry::new_file("existing/nested.txt".into(), 100, 0o644);
    writer.write_entry(&mut data, &file).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

    // Mark the parent directory as already created
    receiver.mark_directory_created("existing");

    // Now the nested file should be immediately ready
    let entry = receiver.next_ready().unwrap().unwrap();
    assert_eq!(entry.name(), "existing/nested.txt");
}

#[test]
fn transfer_stats_has_incremental_fields() {
    let stats = TransferStats {
        files_listed: 0,
        files_transferred: 0,
        bytes_received: 0,
        bytes_sent: 0,
        total_source_bytes: 0,
        metadata_errors: vec![],
        io_error: 0,
        error_count: 0,
        entries_received: 100,
        directories_created: 10,
        directories_failed: 2,
        files_skipped: 5,
        delete_stats: DeleteStats::new(),
        delete_limit_exceeded: false,
        literal_data: 0,
        matched_data: 0,
        redo_count: 0,
    };

    assert_eq!(stats.entries_received, 100);
    assert_eq!(stats.directories_created, 10);
    assert_eq!(stats.directories_failed, 2);
    assert_eq!(stats.files_skipped, 5);
}

mod incremental_receiver_tests {
    use super::*;

    /// Helper: create wire-encoded file list data from entries.
    fn encode_entries(entries: &[FileEntry]) -> Vec<u8> {
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);

        for entry in entries {
            writer.write_entry(&mut data, entry).unwrap();
        }
        writer.write_end(&mut data, None).unwrap();

        data
    }

    /// Helper: create an `IncrementalFileListReceiver` from raw wire data.
    fn make_receiver(data: Vec<u8>) -> super::super::IncrementalFileListReceiver<Cursor<Vec<u8>>> {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);
        ctx.incremental_file_list_receiver(Cursor::new(data))
    }

    #[test]
    fn try_read_one_returns_false_when_finished() {
        // Create a receiver that's already marked as finished
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let flist_reader = protocol::flist::FileListReader::new(protocol);

        // Empty data - will hit EOF immediately
        let empty_data: Vec<u8> = vec![0]; // Single zero byte = end of list marker
        let source = Cursor::new(empty_data);

        let incremental = protocol::flist::IncrementalFileList::new();

        let mut receiver = super::super::IncrementalFileListReceiver {
            flist_reader,
            source,
            incremental,
            finished_reading: true, // Already finished
            entries_read: 0,
            use_qsort: false,
        };

        // Should return false since already finished
        assert!(!receiver.try_read_one().unwrap());
    }

    #[test]
    fn try_read_one_on_empty_list_returns_false() {
        // An empty file list (only the end-of-list marker) should
        // cause try_read_one to hit EOF and return false.
        let data = encode_entries(&[]);
        let mut receiver = make_receiver(data);

        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());
        assert_eq!(receiver.entries_read(), 0);
    }

    #[test]
    fn try_read_one_reads_single_entry() {
        let file = FileEntry::new_file("hello.txt".into(), 42, 0o644);
        let data = encode_entries(&[file]);
        let mut receiver = make_receiver(data);

        // First call reads one entry
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);
        assert_eq!(receiver.ready_count(), 1);
        assert!(!receiver.is_finished_reading());

        // The entry should be available via pop / next_ready
        let entry = receiver.next_ready().unwrap().unwrap();
        assert_eq!(entry.name(), "hello.txt");
        assert_eq!(entry.size(), 42);
    }

    #[test]
    fn try_read_one_reads_entries_one_at_a_time() {
        let entries = vec![
            FileEntry::new_file("a.txt".into(), 10, 0o644),
            FileEntry::new_file("b.txt".into(), 20, 0o644),
            FileEntry::new_file("c.txt".into(), 30, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one at a time
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);
        assert_eq!(receiver.ready_count(), 1);

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 2);
        assert_eq!(receiver.ready_count(), 2);

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 3);
        assert_eq!(receiver.ready_count(), 3);

        // Next call hits end-of-list
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());

        // All three entries should be ready
        let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn try_read_one_after_eof_is_idempotent() {
        let data = encode_entries(&[FileEntry::new_file("only.txt".into(), 1, 0o644)]);
        let mut receiver = make_receiver(data);

        // Read the single entry
        assert!(receiver.try_read_one().unwrap());
        // Hit EOF
        assert!(!receiver.try_read_one().unwrap());
        // Subsequent calls continue to return false
        assert!(!receiver.try_read_one().unwrap());
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());
    }

    #[test]
    fn try_read_one_child_before_parent_stays_pending() {
        // Child file arrives before its parent directory.
        // try_read_one should add it to pending, not ready.
        let entries = vec![
            FileEntry::new_file("subdir/child.txt".into(), 100, 0o644),
            FileEntry::new_directory("subdir".into(), 0o755),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read child first - goes to pending since "subdir" doesn't exist
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 1);

        // Read parent directory - should release child too
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 2);
        assert_eq!(receiver.ready_count(), 2); // dir + file
        assert_eq!(receiver.pending_count(), 0);
    }

    #[test]
    fn try_read_one_with_pre_marked_directory() {
        // Mark a directory as created before reading. A child entry
        // should become immediately ready.
        let entries = vec![FileEntry::new_file("existing/file.txt".into(), 50, 0o644)];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        receiver.mark_directory_created("existing");

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 1);
        assert_eq!(receiver.pending_count(), 0);

        let entry = receiver.next_ready().unwrap().unwrap();
        assert_eq!(entry.name(), "existing/file.txt");
    }

    #[test]
    fn try_read_one_deeply_nested_out_of_order() {
        // Push entries in reverse depth order, then verify resolution.
        let entries = vec![
            FileEntry::new_file("a/b/c/deep.txt".into(), 1, 0o644),
            FileEntry::new_directory("a/b/c".into(), 0o755),
            FileEntry::new_directory("a/b".into(), 0o755),
            FileEntry::new_directory("a".into(), 0o755),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read deep file - pending (no ancestors)
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 1);

        // Read "a/b/c" - pending (parent "a/b" missing)
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 2);

        // Read "a/b" - pending (parent "a" missing)
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 0);
        assert_eq!(receiver.pending_count(), 3);

        // Read "a" - cascading release: a -> a/b -> a/b/c -> deep.txt
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 4);
        assert_eq!(receiver.pending_count(), 0);
    }

    #[test]
    fn try_read_one_interleaved_with_next_ready() {
        let entries = vec![
            FileEntry::new_file("first.txt".into(), 1, 0o644),
            FileEntry::new_file("second.txt".into(), 2, 0o644),
            FileEntry::new_file("third.txt".into(), 3, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one, consume it, read next
        assert!(receiver.try_read_one().unwrap());
        let e1 = receiver.next_ready().unwrap().unwrap();
        assert_eq!(e1.name(), "first.txt");
        assert_eq!(receiver.ready_count(), 0);

        assert!(receiver.try_read_one().unwrap());
        let e2 = receiver.next_ready().unwrap().unwrap();
        assert_eq!(e2.name(), "second.txt");

        assert!(receiver.try_read_one().unwrap());
        let e3 = receiver.next_ready().unwrap().unwrap();
        assert_eq!(e3.name(), "third.txt");

        // No more
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.next_ready().unwrap().is_none());
    }

    #[test]
    fn try_read_one_interleaved_with_drain_ready() {
        let entries = vec![
            FileEntry::new_file("x.txt".into(), 1, 0o644),
            FileEntry::new_file("y.txt".into(), 2, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read both entries
        assert!(receiver.try_read_one().unwrap());
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 2);

        // Drain all at once
        let drained = receiver.drain_ready();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].name(), "x.txt");
        assert_eq!(drained[1].name(), "y.txt");
        assert_eq!(receiver.ready_count(), 0);

        // EOF
        assert!(!receiver.try_read_one().unwrap());
    }

    #[test]
    fn try_read_one_directory_and_children() {
        let entries = vec![
            FileEntry::new_directory("mydir".into(), 0o755),
            FileEntry::new_file("mydir/alpha.txt".into(), 10, 0o644),
            FileEntry::new_file("mydir/beta.txt".into(), 20, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read directory
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 1);

        // Read children - they should be immediately ready since parent exists
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 2);

        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.ready_count(), 3);

        // Verify order
        let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, vec!["mydir", "mydir/alpha.txt", "mydir/beta.txt"]);
    }

    #[test]
    fn try_read_one_is_empty_tracks_state_correctly() {
        let entries = vec![FileEntry::new_file("f.txt".into(), 1, 0o644)];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Not empty initially (haven't read yet, not finished)
        assert!(!receiver.is_finished_reading());

        // Read the entry
        assert!(receiver.try_read_one().unwrap());
        // Not empty: still has a ready entry
        assert!(!receiver.is_empty());

        // Hit EOF
        assert!(!receiver.try_read_one().unwrap());
        // Still not empty: one ready entry remains
        assert!(!receiver.is_empty());

        // Consume the entry
        receiver.next_ready().unwrap();
        // Now truly empty
        assert!(receiver.is_empty());
    }

    #[test]
    fn try_read_one_reads_symlink_entry() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.links = true;
        let ctx = ReceiverContext::new(&handshake, config);

        // Encode a symlink entry with links preserved
        let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);
        writer = writer.with_preserve_links(true);

        let symlink = FileEntry::new_symlink("link.txt".into(), "/target".into());
        writer.write_entry(&mut data, &symlink).unwrap();
        writer.write_end(&mut data, None).unwrap();

        let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(data));

        assert!(receiver.try_read_one().unwrap());
        let entry = receiver.next_ready().unwrap().unwrap();
        assert!(entry.is_symlink());
        assert_eq!(entry.name(), "link.txt");
    }

    #[test]
    fn try_read_one_increments_entries_read() {
        let entries = vec![
            FileEntry::new_file("one.txt".into(), 1, 0o644),
            FileEntry::new_file("two.txt".into(), 2, 0o644),
            FileEntry::new_file("three.txt".into(), 3, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        assert_eq!(receiver.entries_read(), 0);

        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 1);

        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 2);

        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 3);

        // EOF does not increment
        receiver.try_read_one().unwrap();
        assert_eq!(receiver.entries_read(), 3);
    }

    #[test]
    fn try_read_one_partial_then_collect_sorted() {
        let entries = vec![
            FileEntry::new_file("z.txt".into(), 1, 0o644),
            FileEntry::new_file("a.txt".into(), 2, 0o644),
            FileEntry::new_file("m.txt".into(), 3, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one entry via try_read_one
        assert!(receiver.try_read_one().unwrap());
        // Consume it so it doesn't appear in collect_sorted's drain
        let first = receiver.next_ready().unwrap().unwrap();
        assert_eq!(first.name(), "z.txt");

        // Now collect the remaining entries sorted
        let sorted = receiver.collect_sorted().unwrap();
        assert_eq!(sorted.len(), 2);
        // "a.txt" should come before "m.txt" after sorting
        assert_eq!(sorted[0].name(), "a.txt");
        assert_eq!(sorted[1].name(), "m.txt");
    }

    #[test]
    fn mark_finished_prevents_further_reads() {
        let entries = vec![
            FileEntry::new_file("a.txt".into(), 1, 0o644),
            FileEntry::new_file("b.txt".into(), 2, 0o644),
        ];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        // Read one entry
        assert!(receiver.try_read_one().unwrap());
        assert_eq!(receiver.entries_read(), 1);

        // Mark as finished (simulating error recovery)
        receiver.mark_finished();

        // try_read_one should now return false even though data remains
        assert!(!receiver.try_read_one().unwrap());
        assert!(receiver.is_finished_reading());
        assert_eq!(receiver.entries_read(), 1);
    }

    #[test]
    fn try_read_one_stats_are_accessible() {
        let entries = vec![FileEntry::new_file("stat_test.txt".into(), 999, 0o644)];
        let data = encode_entries(&entries);
        let mut receiver = make_receiver(data);

        assert!(receiver.try_read_one().unwrap());
        // Stats should reflect one regular file read
        let stats = receiver.stats();
        assert_eq!(stats.num_files, 1);
        assert_eq!(stats.total_size, 999);
    }
}

#[test]
fn run_pipelined_incremental_compiles() {
    // This test just verifies the method signature is correct
    fn _check_signature<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        ctx: &mut ReceiverContext,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) {
        let _ = ctx.run_pipelined_incremental(reader, writer, PipelineConfig::default(), None);
    }
}

mod create_directory_incremental_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_directory_successfully() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("subdir".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed, None);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(true)); // Returns Some(true) for new dir
        assert!(dest.join("subdir").exists());
        assert_eq!(failed.count(), 0);
    }

    #[test]
    fn skips_child_of_failed_parent() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        let entry = FileEntry::new_directory("failed_parent/child".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();
        failed.mark_failed("failed_parent");

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed, None);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // Returns None for skipped
        assert!(!dest.join("failed_parent/child").exists());
        assert_eq!(failed.count(), 2); // Parent + child marked as failed
    }
}

mod failed_directories_tests {
    use super::FailedDirectories;

    #[test]
    fn failed_directories_empty_has_no_ancestors() {
        let failed = FailedDirectories::new();
        assert!(failed.failed_ancestor("any/path/file.txt").is_none());
    }

    #[test]
    fn failed_directories_marks_and_finds_exact() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert!(failed.failed_ancestor("foo/bar").is_some());
    }

    #[test]
    fn failed_directories_finds_child_of_failed() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert_eq!(
            failed.failed_ancestor("foo/bar/baz/file.txt"),
            Some("foo/bar")
        );
    }

    #[test]
    fn failed_directories_does_not_match_sibling() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("foo/bar");
        assert!(failed.failed_ancestor("foo/other/file.txt").is_none());
    }

    #[test]
    fn failed_directories_counts_failures() {
        let mut failed = FailedDirectories::new();
        assert_eq!(failed.count(), 0);
        failed.mark_failed("a");
        failed.mark_failed("b");
        assert_eq!(failed.count(), 2);
    }
}

#[cfg(feature = "incremental-flist")]
mod incremental_mode_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn failed_directories_skips_nested_children() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("a/b");

        // Direct child
        assert!(failed.failed_ancestor("a/b/file.txt").is_some());
        // Nested child
        assert!(failed.failed_ancestor("a/b/c/d/file.txt").is_some());
        // Sibling - not affected
        assert!(failed.failed_ancestor("a/c/file.txt").is_none());
        // Parent - not affected
        assert!(failed.failed_ancestor("a/file.txt").is_none());
    }

    #[test]
    fn failed_directories_handles_root_level() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("toplevel");

        assert!(failed.failed_ancestor("toplevel/sub/file.txt").is_some());
        assert!(failed.failed_ancestor("other/file.txt").is_none());
    }

    #[test]
    fn stats_tracks_incremental_fields() {
        let stats = TransferStats {
            entries_received: 100,
            directories_created: 20,
            directories_failed: 2,
            files_skipped: 10,
            files_transferred: 68,
            ..Default::default()
        };

        // Verify consistency
        assert_eq!(
            stats.directories_created + stats.directories_failed,
            22 // total directories
        );
    }

    #[test]
    fn create_directory_incremental_nested() {
        let temp = TempDir::new().unwrap();
        let dest = temp.path();

        // Create nested directory
        let entry = FileEntry::new_directory("a/b/c".into(), 0o755);
        let opts = metadata::MetadataOptions::default();
        let mut failed = FailedDirectories::new();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed, None);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(true));
        assert!(dest.join("a/b/c").exists());
    }

    #[test]
    fn failed_directories_propagates_to_deeply_nested() {
        let mut failed = FailedDirectories::new();
        failed.mark_failed("level1");

        // All descendants should be affected
        assert!(failed.failed_ancestor("level1/level2").is_some());
        assert!(failed.failed_ancestor("level1/level2/level3").is_some());
        assert!(
            failed
                .failed_ancestor("level1/level2/level3/file.txt")
                .is_some()
        );
    }

    #[test]
    fn checksum_length_phase1_equals_short_sum_length() {
        assert_eq!(
            PHASE1_CHECKSUM_LENGTH.get(),
            signature::block_size::SHORT_SUM_LENGTH,
        );
        assert_eq!(PHASE1_CHECKSUM_LENGTH.get(), 2);
    }

    #[test]
    fn checksum_length_redo_equals_max_sum_length() {
        assert_eq!(
            REDO_CHECKSUM_LENGTH.get(),
            signature::block_size::MAX_SUM_LENGTH,
        );
        assert_eq!(REDO_CHECKSUM_LENGTH.get(), 16);
    }

    #[test]
    fn checksum_length_phase1_less_than_redo() {
        assert!(PHASE1_CHECKSUM_LENGTH < REDO_CHECKSUM_LENGTH);
    }

    #[test]
    fn transfer_stats_default_values() {
        let stats = TransferStats::default();

        assert_eq!(stats.entries_received, 0);
        assert_eq!(stats.directories_created, 0);
        assert_eq!(stats.directories_failed, 0);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.files_transferred, 0);
        assert_eq!(stats.bytes_received, 0);
    }
}

/// Tests for legacy goodbye handshake (protocol 28/29).
///
/// Protocol 28/29 uses a simpler goodbye sequence than protocol 30+:
/// just NDX_DONE as 4-byte little-endian i32, without NDX_FLIST_EOF
/// or NDX_DEL_STATS messages.
///
/// upstream: main.c:875-906 `read_final_goodbye()`
mod legacy_goodbye_tests {
    use super::*;
    use protocol::codec::{NdxCodec, create_ndx_codec};

    /// NDX_DONE as 4-byte little-endian (-1 = 0xFFFFFFFF).
    const NDX_DONE_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

    /// Creates a `HandshakeResult` for a specific protocol version.
    fn handshake_for(protocol_version: u8) -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// Creates a `ReceiverContext` for a given protocol version.
    fn receiver_for(protocol_version: u8) -> ReceiverContext {
        let handshake = handshake_for(protocol_version);
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        ReceiverContext::new(&handshake, config)
    }

    #[test]
    fn proto28_supports_goodbye_but_not_extended() {
        let ctx = receiver_for(28);
        assert!(ctx.protocol.supports_goodbye_exchange());
        assert!(!ctx.protocol.supports_extended_goodbye());
        assert!(!ctx.protocol.supports_multi_phase());
    }

    #[test]
    fn proto29_supports_goodbye_but_not_extended() {
        let ctx = receiver_for(29);
        assert!(ctx.protocol.supports_goodbye_exchange());
        assert!(!ctx.protocol.supports_extended_goodbye());
        assert!(ctx.protocol.supports_multi_phase());
    }

    #[test]
    fn exchange_phase_done_proto28_single_phase() {
        let ctx = receiver_for(28);

        let mut sender_input = Vec::new();
        sender_input.extend_from_slice(&NDX_DONE_LE); // echo for phase 1
        sender_input.extend_from_slice(&NDX_DONE_LE); // sender's post-loop final

        let mut reader = Cursor::new(sender_input);
        let mut output = Vec::new();
        let mut ndx_write = create_ndx_codec(28);
        let mut ndx_read = create_ndx_codec(28);

        ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
            .unwrap();

        // 2 NDX_DONEs = 8 bytes, all 0xFF
        assert_eq!(output.len(), 8);
        assert_eq!(&output[0..4], &NDX_DONE_LE);
        assert_eq!(&output[4..8], &NDX_DONE_LE);
    }

    #[test]
    fn exchange_phase_done_proto29_two_phases() {
        let ctx = receiver_for(29);

        let mut sender_input = Vec::new();
        for _ in 0..3 {
            sender_input.extend_from_slice(&NDX_DONE_LE);
        }

        let mut reader = Cursor::new(sender_input);
        let mut output = Vec::new();
        let mut ndx_write = create_ndx_codec(29);
        let mut ndx_read = create_ndx_codec(29);

        ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
            .unwrap();

        // 3 NDX_DONEs = 12 bytes
        assert_eq!(output.len(), 12);
        for i in 0..3 {
            assert_eq!(&output[i * 4..(i + 1) * 4], &NDX_DONE_LE);
        }
    }

    #[test]
    fn handle_goodbye_proto28_sends_single_ndx_done() {
        let ctx = receiver_for(28);

        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let mut ndx_write = create_ndx_codec(28);
        let mut ndx_read = create_ndx_codec(28);

        ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
            .unwrap();

        assert_eq!(output.len(), 4);
        assert_eq!(&output[..], &NDX_DONE_LE);
    }

    #[test]
    fn handle_goodbye_proto29_sends_single_ndx_done() {
        let ctx = receiver_for(29);

        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let mut ndx_write = create_ndx_codec(29);
        let mut ndx_read = create_ndx_codec(29);

        ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
            .unwrap();

        assert_eq!(output.len(), 4);
        assert_eq!(&output[..], &NDX_DONE_LE);
    }

    #[test]
    fn legacy_ndx_done_wire_format_matches_read_int() {
        let mut codec = create_ndx_codec(28);
        let mut buf = Vec::new();
        codec.write_ndx_done(&mut buf).unwrap();

        assert_eq!(buf, (-1i32).to_le_bytes());
    }

    #[test]
    fn exchange_phase_done_proto28_reads_all_sender_bytes() {
        let ctx = receiver_for(28);

        let mut sender_input = Vec::new();
        sender_input.extend_from_slice(&NDX_DONE_LE);
        sender_input.extend_from_slice(&NDX_DONE_LE);

        let mut reader = Cursor::new(sender_input);
        let mut output = Vec::new();
        let mut ndx_write = create_ndx_codec(28);
        let mut ndx_read = create_ndx_codec(28);

        ctx.exchange_phase_done(&mut reader, &mut output, &mut ndx_write, &mut ndx_read)
            .unwrap();

        // All sender bytes consumed
        assert_eq!(reader.position(), 8);
    }
}

#[cfg(test)]
mod relative_parents {
    use super::*;
    use protocol::flist::FileEntry;

    fn receiver_with_relative(entries: Vec<FileEntry>) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpRe.".to_owned(),
            flags: ParsedServerFlags {
                relative: true,
                ..Default::default()
            },
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new(&handshake, config);
        ctx.file_list = entries;
        ctx
    }

    fn receiver_without_relative(entries: Vec<FileEntry>) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new(&handshake, config);
        ctx.file_list = entries;
        ctx
    }

    #[test]
    fn ensure_relative_parents_creates_missing_dirs() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![FileEntry::new_file("a/b/c/file.txt".into(), 100, 0o644)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(dest.join("a").is_dir());
        assert!(dest.join("a/b").is_dir());
        assert!(dest.join("a/b/c").is_dir());
    }

    #[test]
    fn ensure_relative_parents_handles_multiple_entries_shared_prefix() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![
            FileEntry::new_file("src/lib/mod.rs".into(), 50, 0o644),
            FileEntry::new_file("src/lib/util.rs".into(), 75, 0o644),
            FileEntry::new_file("src/bin/main.rs".into(), 200, 0o644),
        ];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(dest.join("src").is_dir());
        assert!(dest.join("src/lib").is_dir());
        assert!(dest.join("src/bin").is_dir());
    }

    #[test]
    fn ensure_relative_parents_noop_without_relative_flag() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![FileEntry::new_file("a/b/file.txt".into(), 100, 0o644)];
        let ctx = receiver_without_relative(entries);

        ctx.ensure_relative_parents(dest);

        // Without --relative, no parent directories are created
        assert!(!dest.join("a").exists());
    }

    #[test]
    fn ensure_relative_parents_skips_dot_path() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let entries = vec![
            FileEntry::new_directory(".".into(), 0o755),
            FileEntry::new_file("file.txt".into(), 100, 0o644),
        ];
        let ctx = receiver_with_relative(entries);

        // Should not panic or create anything unexpected
        ctx.ensure_relative_parents(dest);
    }

    #[test]
    fn ensure_relative_parents_handles_directory_entries() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        // Directory entry at "a/b/c" - parents "a/" and "a/b/" should be created
        let entries = vec![FileEntry::new_directory("a/b/c".into(), 0o755)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        assert!(dest.join("a").is_dir());
        assert!(dest.join("a/b").is_dir());
        // "a/b/c" is NOT created by ensure_relative_parents (it's a dir entry,
        // handled by create_directories / create_directory_incremental)
        assert!(!dest.join("a/b/c").exists());
    }

    #[test]
    fn ensure_relative_parents_existing_dirs_not_clobbered() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        // Pre-create directory with a file inside
        std::fs::create_dir_all(dest.join("a/b")).unwrap();
        std::fs::write(dest.join("a/b/existing.txt"), "hello").unwrap();

        let entries = vec![FileEntry::new_file("a/b/c/new.txt".into(), 100, 0o644)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        // Existing content preserved
        assert_eq!(
            std::fs::read_to_string(dest.join("a/b/existing.txt")).unwrap(),
            "hello"
        );
        // New parent created
        assert!(dest.join("a/b/c").is_dir());
    }

    #[test]
    fn ensure_relative_parents_dry_run_creates_nothing() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpRne.".to_owned(),
            flags: ParsedServerFlags {
                relative: true,
                dry_run: true,
                ..Default::default()
            },
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new(&handshake, config);
        ctx.file_list = vec![FileEntry::new_file(
            "deep/nested/file.txt".into(),
            100,
            0o644,
        )];

        ctx.ensure_relative_parents(dest);

        assert!(!dest.join("deep").exists());
    }

    #[test]
    fn ensure_relative_parents_single_component_path() {
        let tmp = test_support::create_tempdir();
        let dest = tmp.path();

        // File at root level - no parent creation needed
        let entries = vec![FileEntry::new_file("file.txt".into(), 100, 0o644)];
        let ctx = receiver_with_relative(entries);

        ctx.ensure_relative_parents(dest);

        // No directories created for a root-level file
        let dir_entries: Vec<_> = std::fs::read_dir(dest).unwrap().collect();
        assert!(dir_entries.is_empty());
    }
}

mod receiver_itemize_tests {
    use super::*;
    use crate::generator::ItemFlags;
    use crate::writer::MsgInfoSender;

    /// A test writer that records MSG_INFO payloads for verification.
    struct MockMsgInfoWriter {
        messages: Vec<Vec<u8>>,
    }

    impl MockMsgInfoWriter {
        fn new() -> Self {
            Self {
                messages: Vec::new(),
            }
        }
    }

    impl Write for MockMsgInfoWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl MsgInfoSender for MockMsgInfoWriter {
        fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
            self.messages.push(data.to_vec());
            Ok(())
        }
    }

    fn receiver_config_with_itemize() -> ServerConfig {
        let mut config = test_config();
        config.flags.info_flags.itemize = true;
        // Server mode (not client mode) to enable emission
        config.connection.client_mode = false;
        config
    }

    #[test]
    fn emit_itemize_new_file_transfer() {
        let handshake = test_handshake();
        let config = receiver_config_with_itemize();
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_file("docs/readme.txt".into(), 1024, 0o644);
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert_eq!(writer.messages.len(), 1);
        let msg = String::from_utf8_lossy(&writer.messages[0]);
        // Receiver uses is_sender=false, producing '>' prefix
        assert_eq!(msg, ">f+++++++++ docs/readme.txt\n");
    }

    #[test]
    fn emit_itemize_updated_file_transfer() {
        let handshake = test_handshake();
        let config = receiver_config_with_itemize();
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_file("data.bin".into(), 512, 0o644);
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert_eq!(writer.messages.len(), 1);
        let msg = String::from_utf8_lossy(&writer.messages[0]);
        assert_eq!(msg, ">f......... data.bin\n");
    }

    #[test]
    fn emit_itemize_directory_creation() {
        let handshake = test_handshake();
        let config = receiver_config_with_itemize();
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_directory("subdir".into(), 0o755);
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert_eq!(writer.messages.len(), 1);
        let msg = String::from_utf8_lossy(&writer.messages[0]);
        assert_eq!(msg, "cd+++++++++ subdir/\n");
    }

    #[test]
    fn emit_itemize_up_to_date_file() {
        let handshake = test_handshake();
        let config = receiver_config_with_itemize();
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_file("unchanged.txt".into(), 256, 0o644);
        // No flags - file is up-to-date, no changes
        let iflags = ItemFlags::from_raw(0);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert_eq!(writer.messages.len(), 1);
        let msg = String::from_utf8_lossy(&writer.messages[0]);
        // No transfer, no changes - dots collapse to spaces
        assert_eq!(msg, ".f          unchanged.txt\n");
    }

    #[test]
    fn emit_itemize_skipped_in_client_mode() {
        let handshake = test_handshake();
        let mut config = receiver_config_with_itemize();
        config.connection.client_mode = true; // Client mode suppresses emission
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert!(writer.messages.is_empty());
    }

    #[test]
    fn emit_itemize_skipped_without_itemize_flag() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.info_flags.itemize = false;
        config.connection.client_mode = false;
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert!(writer.messages.is_empty());
    }

    #[test]
    fn emit_itemize_symlink_with_target() {
        let handshake = test_handshake();
        let config = receiver_config_with_itemize();
        let ctx = ReceiverContext::new(&handshake, config);
        let mut writer = MockMsgInfoWriter::new();

        let entry = FileEntry::new_symlink("mylink".into(), "target".into());
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

        ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

        assert_eq!(writer.messages.len(), 1);
        let msg = String::from_utf8_lossy(&writer.messages[0]);
        assert_eq!(msg, "cL+++++++++ mylink -> target\n");
    }

    #[test]
    fn should_emit_itemize_conditions() {
        let handshake = test_handshake();

        // Server mode + itemize -> true
        let mut config = test_config();
        config.connection.client_mode = false;
        config.flags.info_flags.itemize = true;
        let ctx = ReceiverContext::new(&handshake, config);
        assert!(ctx.should_emit_itemize());

        // Client mode + itemize -> false
        let mut config = test_config();
        config.connection.client_mode = true;
        config.flags.info_flags.itemize = true;
        let ctx = ReceiverContext::new(&handshake, config);
        assert!(!ctx.should_emit_itemize());

        // Server mode + no itemize -> false
        let mut config = test_config();
        config.connection.client_mode = false;
        config.flags.info_flags.itemize = false;
        let ctx = ReceiverContext::new(&handshake, config);
        assert!(!ctx.should_emit_itemize());
    }

    #[test]
    fn msg_info_sender_default_noop() {
        // Verify that a bare Write impl with no MsgInfoSender override
        // uses the default no-op behavior
        struct PlainWriter;
        impl Write for PlainWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl MsgInfoSender for PlainWriter {}

        let mut w = PlainWriter;
        // Default impl should succeed silently
        w.send_msg_info(b"test data").unwrap();
    }
}

#[test]
fn path_contains_dot_dot_simple_traversal() {
    use std::path::Path;
    assert!(super::quick_check::path_contains_dot_dot(Path::new(
        "../etc/passwd"
    )));
}

#[test]
fn path_contains_dot_dot_mid_path() {
    use std::path::Path;
    assert!(super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/../../../etc/passwd"
    )));
}

#[test]
fn path_contains_dot_dot_trailing() {
    use std::path::Path;
    assert!(super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/.."
    )));
}

#[test]
fn path_contains_dot_dot_clean_path() {
    use std::path::Path;
    assert!(!super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/c"
    )));
}

#[test]
fn path_contains_dot_dot_dot_only() {
    use std::path::Path;
    // Single "." is not ".."
    assert!(!super::quick_check::path_contains_dot_dot(Path::new(".")));
}

#[test]
fn path_contains_dot_dot_embedded_dots_in_name() {
    use std::path::Path;
    // "..." is not ".." - it's a normal filename
    assert!(!super::quick_check::path_contains_dot_dot(Path::new(
        "a/.../b"
    )));
}

#[test]
fn path_contains_dot_dot_double_dotdot() {
    use std::path::Path;
    assert!(super::quick_check::path_contains_dot_dot(Path::new(
        "a/../../b"
    )));
}

mod sanitize_file_list {
    use super::*;

    fn receiver_with_trust(entries: Vec<FileEntry>, trust_sender: bool) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            trust_sender,
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new(&handshake, config);
        ctx.file_list = entries;
        ctx
    }

    fn receiver_with_trust_and_relative(
        entries: Vec<FileEntry>,
        trust_sender: bool,
    ) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            trust_sender,
            flags: ParsedServerFlags {
                relative: true,
                ..Default::default()
            },
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new(&handshake, config);
        ctx.file_list = entries;
        ctx
    }

    #[test]
    fn safe_paths_kept_when_untrusted() {
        let entries = vec![
            FileEntry::new_file("hello.txt".into(), 10, 0o644),
            FileEntry::new_file("subdir/nested.txt".into(), 20, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);
        assert_eq!(ctx.file_list.len(), 2);
    }

    #[test]
    fn absolute_path_rejected_when_untrusted() {
        let entries = vec![
            FileEntry::new_file("safe.txt".into(), 10, 0o644),
            FileEntry::new_file("/etc/passwd".into(), 20, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 1);
        assert_eq!(ctx.file_list.len(), 1);
        assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "safe.txt");
    }

    #[test]
    fn dot_dot_path_rejected_when_untrusted() {
        let entries = vec![
            FileEntry::new_file("ok.txt".into(), 10, 0o644),
            FileEntry::new_file("../escape.txt".into(), 20, 0o644),
            FileEntry::new_file("sub/../../escape2.txt".into(), 30, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 2);
        assert_eq!(ctx.file_list.len(), 1);
        assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "ok.txt");
    }

    #[test]
    fn absolute_path_allowed_when_trusted() {
        let entries = vec![
            FileEntry::new_file("safe.txt".into(), 10, 0o644),
            FileEntry::new_file("/etc/passwd".into(), 20, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, true);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);
        assert_eq!(ctx.file_list.len(), 2);
    }

    #[test]
    fn dot_dot_path_allowed_when_trusted() {
        let entries = vec![
            FileEntry::new_file("ok.txt".into(), 10, 0o644),
            FileEntry::new_file("../escape.txt".into(), 20, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, true);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);
        assert_eq!(ctx.file_list.len(), 2);
    }

    #[test]
    fn absolute_path_allowed_with_relative_flag() {
        // upstream: absolute paths are allowed when --relative is active
        let entries = vec![FileEntry::new_file("/rooted/file.txt".into(), 10, 0o644)];
        let mut ctx = receiver_with_trust_and_relative(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);
        // Leading slashes are stripped in --relative mode
        assert!(!ctx.file_list[0].path().has_root());
    }

    #[test]
    fn all_unsafe_entries_removed() {
        let entries = vec![
            FileEntry::new_file("/abs1".into(), 10, 0o644),
            FileEntry::new_file("../up1".into(), 20, 0o644),
            FileEntry::new_file("/abs2".into(), 30, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 3);
        assert!(ctx.file_list.is_empty());
    }

    #[test]
    fn trust_sender_skips_all_checks() {
        let entries = vec![
            FileEntry::new_file("/abs".into(), 10, 0o644),
            FileEntry::new_file("../dotdot".into(), 20, 0o644),
            FileEntry::new_file("a/../../escape".into(), 30, 0o644),
            FileEntry::new_file("safe.txt".into(), 40, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, true);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);
        assert_eq!(ctx.file_list.len(), 4);
    }

    #[test]
    fn empty_file_list_returns_zero() {
        let mut ctx = receiver_with_trust(vec![], false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);

        let mut ctx_trusted = receiver_with_trust(vec![], true);
        let removed = ctx_trusted.sanitize_file_list();
        assert_eq!(removed, 0);
    }

    #[test]
    fn directories_with_dot_dot_rejected() {
        let entries = vec![
            FileEntry::new_directory("../evil_dir".into(), 0o755),
            FileEntry::new_directory("safe_dir".into(), 0o755),
        ];
        let mut ctx = receiver_with_trust(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 1);
        assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "safe_dir");
    }

    /// On Windows, `Path::has_root()` is false for drive-relative paths such
    /// as `C:foo`, but `dest_dir.join("C:foo")` discards `dest_dir` entirely
    /// (`Path::join` semantics). Without an additional check, an untrusted
    /// sender could escape the destination tree by emitting a wire path
    /// starting with a drive letter, UNC prefix, or `\\?\` extended prefix.
    #[cfg(windows)]
    #[test]
    fn windows_drive_relative_path_rejected_when_untrusted() {
        let entries = vec![
            FileEntry::new_file("safe.txt".into(), 10, 0o644),
            FileEntry::new_file("C:foo".into(), 20, 0o644),
            FileEntry::new_file(r"C:\absolute".into(), 30, 0o644),
            FileEntry::new_file(r"\\server\share\file".into(), 40, 0o644),
            FileEntry::new_file(r"\\?\C:\verbatim".into(), 50, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, false);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 4);
        assert_eq!(ctx.file_list.len(), 1);
        assert_eq!(ctx.file_list[0].path().to_str().unwrap(), "safe.txt");
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_relative_path_allowed_when_trusted() {
        let entries = vec![
            FileEntry::new_file("safe.txt".into(), 10, 0o644),
            FileEntry::new_file("C:foo".into(), 20, 0o644),
        ];
        let mut ctx = receiver_with_trust(entries, true);
        let removed = ctx.sanitize_file_list();
        assert_eq!(removed, 0);
        assert_eq!(ctx.file_list.len(), 2);
    }
}

/// Tests for receiver input multiplex activation by mode and protocol version.
///
/// upstream: main.c:1342-1343 - client receiver activates at protocol >= 23
/// upstream: main.c:1167-1168 - server receiver activates at protocol >= 30
mod input_multiplex_tests {
    use super::*;

    fn receiver_with_client_mode(protocol_version: u8, client_mode: bool) -> ReceiverContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        config.connection.client_mode = client_mode;
        ReceiverContext::new(&handshake, config)
    }

    #[test]
    fn client_mode_protocol_28_activates_input_multiplex() {
        // upstream: main.c:1342-1343 - protocol >= 23 activates
        let ctx = receiver_with_client_mode(28, true);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn client_mode_protocol_29_activates_input_multiplex() {
        let ctx = receiver_with_client_mode(29, true);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn client_mode_protocol_30_activates_input_multiplex() {
        let ctx = receiver_with_client_mode(30, true);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn client_mode_protocol_32_activates_input_multiplex() {
        let ctx = receiver_with_client_mode(32, true);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn server_mode_protocol_28_does_not_activate_input_multiplex() {
        // upstream: main.c:1167-1168 - server only activates for >= 30
        let ctx = receiver_with_client_mode(28, false);
        assert!(!ctx.should_activate_input_multiplex());
    }

    #[test]
    fn server_mode_protocol_29_does_not_activate_input_multiplex() {
        let ctx = receiver_with_client_mode(29, false);
        assert!(!ctx.should_activate_input_multiplex());
    }

    #[test]
    fn server_mode_protocol_30_activates_input_multiplex() {
        let ctx = receiver_with_client_mode(30, false);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn server_mode_protocol_32_activates_input_multiplex() {
        let ctx = receiver_with_client_mode(32, false);
        assert!(ctx.should_activate_input_multiplex());
    }
}

/// Minimal writer that discards output and provides no-op `MsgInfoSender`.
struct TestDeletionWriter;

impl Write for TestDeletionWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl crate::writer::MsgInfoSender for TestDeletionWriter {
    fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn receiver_filter_chain_protects_from_deletion() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Create files at destination (extra files that should be deleted)
    std::fs::write(dest.join("normal.txt"), b"delete me").unwrap();
    std::fs::write(dest.join("protected.conf"), b"keep me").unwrap();
    std::fs::write(dest.join("source.txt"), b"from sender").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new(&handshake, config);

    // File list includes "." and "source.txt" - anything else at dest is extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("source.txt".into(), 11, 0o644));

    // Set up filter chain with protect rule for *.conf
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::protect("*.conf")]).unwrap();
    ctx.set_filter_chain(::filters::FilterChain::new(global));

    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx.delete_extraneous_files(dest, &mut writer).unwrap();

    // normal.txt should be deleted (not in file list, not protected)
    assert!(
        !dest.join("normal.txt").exists(),
        "normal.txt should be deleted"
    );

    // protected.conf should survive due to protect rule
    assert!(
        dest.join("protected.conf").exists(),
        "protected.conf should be protected from deletion"
    );

    // source.txt should survive (it's in the file list)
    assert!(dest.join("source.txt").exists());

    assert!(stats.files >= 1); // At least normal.txt was deleted
}

#[test]
fn receiver_filter_chain_empty_allows_all_deletions() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("file1.txt"), b"data1").unwrap();
    std::fs::write(dest.join("file2.log"), b"data2").unwrap();
    std::fs::write(dest.join("keep.txt"), b"keep").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new(&handshake, config);

    // File list has "." and "keep.txt" - file1/file2 are extraneous
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 4, 0o644));

    // Empty filter chain - all deletions should proceed
    let mut writer = TestDeletionWriter;
    let (stats, _) = ctx.delete_extraneous_files(dest, &mut writer).unwrap();

    assert!(!dest.join("file1.txt").exists());
    assert!(!dest.join("file2.log").exists());
    assert!(dest.join("keep.txt").exists());
    assert_eq!(stats.files, 2);
}

#[test]
fn receiver_set_and_get_filter_chain() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Default filter chain should be empty
    assert!(ctx.filter_chain().is_empty());

    // Set a chain with rules
    let global =
        ::filters::FilterSet::from_rules([::filters::FilterRule::exclude("*.bak")]).unwrap();
    let chain = ::filters::FilterChain::new(global);
    ctx.set_filter_chain(chain);

    assert!(!ctx.filter_chain().is_empty());
}

/// Creates a `ReceiverContext` configured for hardlink testing with protocol 32.
fn receiver_with_hardlinks(entries: Vec<FileEntry>) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = entries;
    ctx
}

/// Helper to create a hardlink leader entry with appropriate flags.
fn make_hlink_leader(name: &str, size: u64, gnum: u32) -> FileEntry {
    use protocol::flist::{XMIT_HLINK_FIRST, XMIT_HLINKED};
    let mut entry = FileEntry::new_file(name.into(), size, 0o644);
    entry.flags_mut().extended |= XMIT_HLINKED | XMIT_HLINK_FIRST;
    entry.set_hardlink_idx(gnum);
    entry
}

/// Helper to create a hardlink follower entry with appropriate flags.
fn make_hlink_follower(name: &str, size: u64, gnum: u32) -> FileEntry {
    use protocol::flist::XMIT_HLINKED;
    let mut entry = FileEntry::new_file(name.into(), size, 0o644);
    entry.flags_mut().extended |= XMIT_HLINKED;
    entry.set_hardlink_idx(gnum);
    entry
}

#[test]
fn create_hardlinks_links_follower_to_leader() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Create the leader file on disk (simulating completed transfer)
    let leader_file = dest.join("leader.txt");
    std::fs::write(&leader_file, "shared content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 42),
        make_hlink_follower("follower.txt", 14, 42),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    let follower_file = dest.join("follower.txt");
    assert!(follower_file.exists(), "follower should be created");
    assert_eq!(
        std::fs::read_to_string(&follower_file).unwrap(),
        "shared content"
    );
}

#[cfg(unix)]
#[test]
fn create_hardlinks_shares_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader_file = dest.join("a.txt");
    std::fs::write(&leader_file, "inode check").unwrap();

    let entries = vec![
        make_hlink_leader("a.txt", 11, 100),
        make_hlink_follower("b.txt", 11, 100),
        make_hlink_follower("c.txt", 11, 100),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    let meta_a = std::fs::metadata(dest.join("a.txt")).unwrap();
    let meta_b = std::fs::metadata(dest.join("b.txt")).unwrap();
    let meta_c = std::fs::metadata(dest.join("c.txt")).unwrap();

    assert_eq!(meta_a.ino(), meta_b.ino(), "b should share inode with a");
    assert_eq!(meta_a.ino(), meta_c.ino(), "c should share inode with a");
    assert_eq!(meta_a.nlink(), 3, "nlink should be 3");
}

#[test]
fn create_hardlinks_across_directories() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::create_dir_all(dest.join("dir_a")).unwrap();
    let leader_file = dest.join("dir_a/file.txt");
    std::fs::write(&leader_file, "cross-dir").unwrap();

    let entries = vec![
        make_hlink_leader("dir_a/file.txt", 9, 50),
        make_hlink_follower("dir_b/file.txt", 9, 50),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    let follower = dest.join("dir_b/file.txt");
    assert!(
        follower.exists(),
        "follower in different dir should be created"
    );
    assert_eq!(std::fs::read_to_string(&follower).unwrap(), "cross-dir");
}

#[test]
fn create_hardlinks_multiple_groups() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("g1_leader.txt"), "group1").unwrap();
    std::fs::write(dest.join("g2_leader.txt"), "group2").unwrap();

    let entries = vec![
        make_hlink_leader("g1_leader.txt", 6, 10),
        make_hlink_follower("g1_follower.txt", 6, 10),
        make_hlink_leader("g2_leader.txt", 6, 20),
        make_hlink_follower("g2_follower.txt", 6, 20),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    assert_eq!(
        std::fs::read_to_string(dest.join("g1_follower.txt")).unwrap(),
        "group1"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("g2_follower.txt")).unwrap(),
        "group2"
    );
}

#[cfg(unix)]
#[test]
fn create_hardlinks_skips_already_linked() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader = dest.join("leader.txt");
    let follower = dest.join("follower.txt");
    std::fs::write(&leader, "already linked").unwrap();
    std::fs::hard_link(&leader, &follower).unwrap();

    let leader_ino = std::fs::metadata(&leader).unwrap().ino();
    let follower_ino = std::fs::metadata(&follower).unwrap().ino();
    assert_eq!(leader_ino, follower_ino);

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 77),
        make_hlink_follower("follower.txt", 14, 77),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    // Should remain linked (no change)
    let meta = std::fs::metadata(&follower).unwrap();
    assert_eq!(meta.ino(), leader_ino);
}

#[test]
fn create_hardlinks_replaces_existing_file() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader = dest.join("leader.txt");
    let follower = dest.join("follower.txt");
    std::fs::write(&leader, "correct").unwrap();
    std::fs::write(&follower, "wrong content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 7, 88),
        make_hlink_follower("follower.txt", 7, 88),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    assert_eq!(std::fs::read_to_string(&follower).unwrap(), "correct");
}

#[test]
fn create_hardlinks_skipped_when_disabled() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("leader.txt"), "content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 7, 1),
        make_hlink_follower("follower.txt", 7, 1),
    ];

    // Create ctx WITHOUT hard_links flag
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: false,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = entries;

    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    // Follower should NOT be created
    assert!(
        !dest.join("follower.txt").exists(),
        "follower should not be created when hard_links is disabled"
    );
}

#[test]
fn create_hardlinks_skipped_in_dry_run() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("leader.txt"), "content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 7, 1),
        make_hlink_follower("follower.txt", 7, 1),
    ];

    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHnre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            dry_run: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);
    ctx.file_list = entries;

    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    assert!(
        !dest.join("follower.txt").exists(),
        "follower should not be created in dry_run mode"
    );
}

#[test]
fn create_hardlinks_follower_without_leader_is_skipped() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Only a follower, no leader in file list
    let entries = vec![make_hlink_follower("orphan.txt", 10, 999)];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    // Should not create any file (leader missing)
    assert!(
        !dest.join("orphan.txt").exists(),
        "orphan follower should not create a file"
    );
}

/// Verifies that the HardlinkApplyTracker is initialized when hard_links is enabled.
#[test]
fn tracker_initialized_when_hard_links_enabled() {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let ctx = ReceiverContext::new(&handshake, config);
    assert!(
        ctx.hardlink_tracker.is_some(),
        "tracker should be initialized when hard_links is enabled"
    );
}

/// Verifies that the tracker is NOT initialized when hard_links is disabled.
#[test]
fn tracker_not_initialized_when_hard_links_disabled() {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: false,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let ctx = ReceiverContext::new(&handshake, config);
    assert!(
        ctx.hardlink_tracker.is_none(),
        "tracker should not be initialized when hard_links is disabled"
    );
}

/// Verifies that create_hardlinks populates the tracker's leader map and
/// that the tracker is restored (not consumed) after the operation.
#[test]
fn create_hardlinks_populates_tracker() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("a.txt"), "content-a").unwrap();
    std::fs::write(dest.join("b.txt"), "content-b").unwrap();

    let entries = vec![
        make_hlink_leader("a.txt", 9, 10),
        make_hlink_follower("a_link.txt", 9, 10),
        make_hlink_leader("b.txt", 9, 20),
        make_hlink_follower("b_link.txt", 9, 20),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    // Verify follower links were created.
    assert!(
        dest.join("a_link.txt").exists(),
        "follower a_link.txt should exist"
    );
    assert!(
        dest.join("b_link.txt").exists(),
        "follower b_link.txt should exist"
    );

    // Verify tracker is preserved (not consumed by take()).
    let tracker = ctx
        .hardlink_tracker
        .as_ref()
        .expect("tracker should be restored");
    assert_eq!(
        tracker.leader_count(),
        2,
        "tracker should have 2 leaders recorded"
    );
    assert_eq!(
        tracker.deferred_count(),
        0,
        "no deferred followers should remain"
    );
}

/// Verifies that the tracker correctly tracks leaders across multiple
/// create_hardlinks calls (e.g., incremental file list segments).
#[cfg(unix)]
#[test]
fn create_hardlinks_tracker_preserves_state_across_calls() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    // First call: leader only, no followers yet.
    std::fs::write(dest.join("leader.txt"), "persistent").unwrap();

    let entries_1 = vec![make_hlink_leader("leader.txt", 10, 50)];
    let mut ctx = receiver_with_hardlinks(entries_1);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    // Tracker should have the leader recorded.
    let tracker = ctx.hardlink_tracker.as_ref().unwrap();
    assert_eq!(tracker.leader_count(), 1);

    // Now simulate a second segment with a follower referencing the same leader.
    ctx.file_list = vec![
        make_hlink_leader("leader.txt", 10, 50),
        make_hlink_follower("follower.txt", 10, 50),
    ];
    ctx.create_hardlinks(dest, &mut writer);

    // Follower should be linked.
    assert!(dest.join("follower.txt").exists());
    let leader_ino = std::fs::metadata(dest.join("leader.txt")).unwrap().ino();
    let follower_ino = std::fs::metadata(dest.join("follower.txt")).unwrap().ino();
    assert_eq!(
        leader_ino, follower_ino,
        "follower should share inode with leader"
    );
}

/// Verifies that three followers in the same group all share the leader's inode.
#[cfg(unix)]
#[test]
fn create_hardlinks_multiple_followers_same_group() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("original.txt"), "shared data").unwrap();

    let entries = vec![
        make_hlink_leader("original.txt", 11, 7),
        make_hlink_follower("copy1.txt", 11, 7),
        make_hlink_follower("copy2.txt", 11, 7),
        make_hlink_follower("copy3.txt", 11, 7),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    ctx.create_hardlinks(dest, &mut writer);

    let leader_ino = std::fs::metadata(dest.join("original.txt")).unwrap().ino();
    for name in &["copy1.txt", "copy2.txt", "copy3.txt"] {
        let follower_ino = std::fs::metadata(dest.join(name)).unwrap().ino();
        assert_eq!(
            leader_ino, follower_ino,
            "{name} should share inode with leader"
        );
    }

    // Link count should be 4 (1 leader + 3 followers).
    let nlink = std::fs::metadata(dest.join("original.txt"))
        .unwrap()
        .nlink();
    assert_eq!(nlink, 4, "link count should be 4");
}

#[test]
fn daemon_filter_set_empty_when_no_rules() {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new(&handshake, config);
    assert!(ctx.daemon_filter_set().is_none());
}

#[test]
fn daemon_filter_set_built_from_config_rules() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.tmp".to_string(),
        anchored: false,
        directory_only: false,
        no_inherit: false,
        cvs_exclude: false,
        word_split: false,
        exclude_from_merge: false,
        xattr_only: false,
        sender_side: false,
        receiver_side: false,
        perishable: false,
        negate: false,
    }];
    let ctx = ReceiverContext::new(&handshake, config);

    let filters = ctx.daemon_filter_set();
    assert!(
        filters.is_some(),
        "daemon filter set should be built from rules"
    );

    let filters = filters.unwrap();
    // *.tmp should be excluded
    assert!(
        !filters.allows(std::path::Path::new("test.tmp"), false),
        "*.tmp should be excluded by daemon filter"
    );
    // *.txt should be allowed (no matching rule)
    assert!(
        filters.allows(std::path::Path::new("test.txt"), false),
        "*.txt should be allowed through daemon filter"
    );
}

#[test]
fn daemon_filter_set_include_and_exclude() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![
        FilterRuleWireFormat {
            rule_type: RuleType::Include,
            pattern: "*.rs".to_string(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "*".to_string(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
        },
    ];
    let ctx = ReceiverContext::new(&handshake, config);

    let filters = ctx.daemon_filter_set().unwrap();
    // *.rs should be included (explicit include before wildcard exclude)
    assert!(
        filters.allows(std::path::Path::new("main.rs"), false),
        "*.rs should be included by daemon filter"
    );
    // *.txt should be excluded (wildcard exclude)
    assert!(
        !filters.allows(std::path::Path::new("readme.txt"), false),
        "*.txt should be excluded by daemon filter"
    );
}

#[test]
fn daemon_filter_set_anchored_pattern() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "/secret".to_string(),
        anchored: true,
        directory_only: false,
        no_inherit: false,
        cvs_exclude: false,
        word_split: false,
        exclude_from_merge: false,
        xattr_only: false,
        sender_side: false,
        receiver_side: false,
        perishable: false,
        negate: false,
    }];
    let ctx = ReceiverContext::new(&handshake, config);

    let filters = ctx.daemon_filter_set().unwrap();
    // /secret should be excluded (anchored)
    assert!(
        !filters.allows(std::path::Path::new("secret"), false),
        "anchored /secret should be excluded"
    );
    // nested/secret should be allowed (anchored patterns only match at root)
    assert!(
        filters.allows(std::path::Path::new("nested/secret"), false),
        "nested/secret should be allowed (anchored only matches root)"
    );
}

#[test]
fn daemon_filter_rules_prepended_to_receiver_deletion_chain() {
    // Verify that daemon_filter_rules from config are prepended to
    // wire rules when building the filter chain for deletion.
    // This is tested indirectly by verifying the daemon_filter_set
    // is available and that the setup_transfer code path handles
    // the daemon_filter_rules field.
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "secret_*".to_string(),
        anchored: false,
        directory_only: false,
        no_inherit: false,
        cvs_exclude: false,
        word_split: false,
        exclude_from_merge: false,
        xattr_only: false,
        sender_side: false,
        receiver_side: false,
        perishable: false,
        negate: false,
    }];
    let ctx = ReceiverContext::new(&handshake, config);

    // Daemon filter set should reject secret_ files
    let filters = ctx.daemon_filter_set().unwrap();
    assert!(
        !filters.allows(std::path::Path::new("secret_data.bin"), false),
        "secret_data.bin should be excluded by daemon filter"
    );
    assert!(
        filters.allows(std::path::Path::new("public_data.bin"), false),
        "public_data.bin should be allowed through daemon filter"
    );
}

// Protocol 28/29 io_error after file list (flist.c:2738-2742)

/// Verifies that `receive_file_list` reads the 4-byte LE io_error flag
/// after the file list end marker for protocol < 30.
///
/// upstream: flist.c:2738-2742 - the sender writes `write_int(f, io_error)`
/// after the id lists. Without this read, subsequent wire data is misaligned,
/// causing "received request to transfer non-regular file" errors.
#[test]
fn receive_file_list_reads_io_error_for_proto28() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire bytes: 0x00 end marker + 4-byte LE io_error (value 3 = IOERR_GENERAL | IOERR_DEL_LIMIT)
    let io_error_value: i32 = 3;
    let mut wire = vec![0x00u8]; // end marker
    wire.extend_from_slice(&io_error_value.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0, "empty file list should have 0 entries");
    assert_eq!(
        ctx.flist_io_error, io_error_value,
        "io_error should be read from wire"
    );
}

/// Verifies that `receive_file_list` reads io_error for protocol 29 (also < 30).
#[test]
fn receive_file_list_reads_io_error_for_proto29() {
    let handshake = test_handshake_with_protocol(29);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(29u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire: end marker + io_error = 0 (no error)
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&0i32.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(ctx.flist_io_error, 0, "zero io_error should not set field");
}

/// Verifies that protocol >= 30 does NOT read the 4-byte io_error (uses
/// MSG_IO_ERROR multiplexed frames instead).
#[test]
fn receive_file_list_skips_io_error_for_proto30() {
    let handshake = test_handshake_with_protocol(30);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(30u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire: just end marker, no io_error bytes. If the code tried to read
    // 4 more bytes it would fail with UnexpectedEof.
    let wire = vec![0x00u8];
    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(ctx.flist_io_error, 0);
}

/// Verifies that `ignore_errors` prevents accumulating the io_error flag.
#[test]
fn receive_file_list_ignore_errors_suppresses_io_error() {
    let handshake = test_handshake_with_protocol(28);
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(28u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            numeric_ids: true,
            ..Default::default()
        },
        deletion: crate::config::DeletionConfig {
            ignore_errors: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Wire: end marker + io_error = 7
    let mut wire = vec![0x00u8];
    wire.extend_from_slice(&7i32.to_le_bytes());

    let mut cursor = Cursor::new(wire);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        ctx.flist_io_error, 0,
        "ignore_errors should suppress io_error accumulation"
    );
}
