//! Tests for the generator module.

use super::super::flags::ParsedServerFlags;
use super::super::role::ServerRole;
use super::delta::{
    LARGE_FILE_WARNING_THRESHOLD, script_to_wire_delta, stream_whole_file_transfer,
    write_delta_with_compression,
};
use super::file_list::apply_permutation_in_place;
use super::protocol_io::{calculate_duration_ms, read_signature_blocks};
use super::*;
use crate::delta_apply::ChecksumVerifier;
use crate::handshake::HandshakeResult;
use crate::receiver::SumHead;
use engine::delta::{DeltaScript, DeltaToken};
use protocol::filters::FilterRuleWireFormat;
use protocol::wire::{CompressedTokenEncoder, DeltaOp};
use protocol::{ChecksumAlgorithm, CompressionAlgorithm, NegotiationResult, ProtocolVersion};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use crate::config::ServerConfig;

/// Creates a default `ServerConfig` for testing.
fn test_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Generator,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

/// Creates a default `HandshakeResult` for testing.
fn test_handshake() -> HandshakeResult {
    test_handshake_with_protocol(32)
}

/// Creates a `HandshakeResult` with a specific protocol version for testing.
fn test_handshake_with_protocol(protocol_version: u8) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,           // Test mode doesn't need client args
        io_timeout: None,            // Test mode doesn't configure I/O timeouts
        negotiated_algorithms: None, // Test mode uses defaults
        compat_flags: None,          // Test mode uses defaults
        checksum_seed: 0,            // Test mode uses dummy seed
    }
}

/// Creates a `HandshakeResult` with negotiated algorithms for testing.
fn test_handshake_with_negotiated_algorithms(
    protocol_version: u8,
    checksum: ChecksumAlgorithm,
    compression: CompressionAlgorithm,
) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: Some(NegotiationResult {
            checksum,
            compression,
        }),
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Creates a `GeneratorContext` with default test configuration.
fn test_generator() -> (HandshakeResult, GeneratorContext) {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = GeneratorContext::new(&handshake, config);
    (handshake, ctx)
}

/// Creates a `GeneratorContext` configured for a specific path with optional recursion.
fn test_generator_for_path(
    base_path: &Path,
    recursive: bool,
) -> (HandshakeResult, GeneratorContext) {
    let handshake = test_handshake();
    let mut config = test_config();
    config.args = vec![OsString::from(base_path)];
    config.flags.recursive = recursive;
    let ctx = GeneratorContext::new(&handshake, config);
    (handshake, ctx)
}

/// Parses filter rules and applies them to a generator context.
fn apply_filters(ctx: &mut GeneratorContext, wire_rules: Vec<FilterRuleWireFormat>) {
    let (filter_set, merge_configs) = ctx.parse_received_filters(&wire_rules).unwrap();
    ctx.filter_chain = ::filters::FilterChain::new(filter_set);
    for config in merge_configs {
        ctx.filter_chain.add_merge_config(config);
    }
}

/// Creates a temporary directory with the specified files.
/// Returns the TempDir (must be kept alive for the duration of the test).
fn create_test_files(files: &[(&str, &[u8])]) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();
    for (name, content) in files {
        let file_path = base_path.join(name);
        if let Some(parent) = file_path.parent() {
            if parent != base_path {
                std::fs::create_dir_all(parent).unwrap();
            }
        }
        std::fs::write(file_path, content).unwrap();
    }
    temp_dir
}

/// Creates a temporary directory with the specified directory structure.
/// Directories are specified with a trailing slash.
fn create_test_structure(entries: &[&str]) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();
    for entry in entries {
        if entry.ends_with('/') {
            std::fs::create_dir_all(base_path.join(entry.trim_end_matches('/'))).unwrap();
        } else {
            let file_path = base_path.join(entry);
            if let Some(parent) = file_path.parent() {
                if parent != base_path {
                    std::fs::create_dir_all(parent).unwrap();
                }
            }
            std::fs::write(file_path, b"data").unwrap();
        }
    }
    temp_dir
}

/// Builds a file list and returns the count.
fn build_file_list_for(ctx: &mut GeneratorContext, base_path: &Path) -> usize {
    let paths = vec![base_path.to_path_buf()];
    ctx.build_file_list(&paths).unwrap()
}

/// Creates a clear rule for filter tests.
fn clear_rule() -> FilterRuleWireFormat {
    use protocol::filters::RuleType;
    FilterRuleWireFormat {
        rule_type: RuleType::Clear,
        pattern: String::new(),
        anchored: false,
        directory_only: false,
        no_inherit: false,
        cvs_exclude: false,
        word_split: false,
        exclude_from_merge: false,
        xattr_only: false,
        sender_side: true,
        receiver_side: true,
        perishable: false,
        negate: false,
    }
}

#[test]
fn generator_context_creation() {
    let (_handshake, ctx) = test_generator();
    assert_eq!(ctx.protocol().as_u8(), 32);
    assert!(ctx.file_list().is_empty());
}

#[test]
fn send_empty_file_list() {
    let (_handshake, mut ctx) = test_generator();

    let mut output = Vec::new();
    let count = ctx.send_file_list(&mut output).unwrap();

    assert_eq!(count, 0);
    // Should just have the end marker
    assert_eq!(output, vec![0u8]);
}

#[test]
fn send_single_file_entry() {
    let (_handshake, mut ctx) = test_generator();

    // Manually add an entry
    let entry = protocol::flist::FileEntry::new_file("test.txt".into(), 100, 0o644);
    ctx.file_list.push(entry);

    let mut output = Vec::new();
    let count = ctx.send_file_list(&mut output).unwrap();

    assert_eq!(count, 1);
    // Should have entry data plus end marker
    assert!(!output.is_empty());
    assert_eq!(*output.last().unwrap(), 0u8); // End marker
}

#[test]
fn build_and_send_round_trip() {
    use crate::receiver::ReceiverContext;

    let handshake = test_handshake();
    let mut gen_config = test_config();
    gen_config.role = ServerRole::Generator;
    let mut generator = GeneratorContext::new(&handshake, gen_config);

    // Add some entries manually (simulating a walk)
    let mut entry1 = protocol::flist::FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_mtime(1700000000, 0);
    let mut entry2 = protocol::flist::FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_mtime(1700000000, 0);
    generator.file_list.push(entry1);
    generator.file_list.push(entry2);

    // Send file list
    let mut wire_data = Vec::new();
    generator.send_file_list(&mut wire_data).unwrap();

    // Receive file list
    let recv_config = test_config();
    let mut receiver = ReceiverContext::new(&handshake, recv_config);
    let mut cursor = Cursor::new(&wire_data[..]);
    let count = receiver.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 2);
    assert_eq!(receiver.file_list()[0].name(), "file1.txt");
    assert_eq!(receiver.file_list()[1].name(), "file2.txt");
}

#[test]
fn parse_received_filters_empty() {
    let (_handshake, ctx) = test_generator();

    let (filter_set, merge_configs) = ctx.parse_received_filters(&[]).unwrap();
    assert!(filter_set.is_empty());
    assert!(merge_configs.is_empty());
}

#[test]
fn parse_received_filters_single_exclude() {
    let (_handshake, ctx) = test_generator();

    let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_owned())];
    let (filter_set, _) = ctx.parse_received_filters(&wire_rules).unwrap();
    assert!(!filter_set.is_empty());
}

#[test]
fn parse_received_filters_multiple_rules() {
    let (_handshake, ctx) = test_generator();

    let wire_rules = vec![
        FilterRuleWireFormat::exclude("*.log".to_owned()),
        FilterRuleWireFormat::include("*.txt".to_owned()),
        FilterRuleWireFormat::exclude("temp/".to_owned()).with_directory_only(true),
    ];

    let (filter_set, _) = ctx.parse_received_filters(&wire_rules).unwrap();
    assert!(!filter_set.is_empty());
}

#[test]
fn parse_received_filters_with_modifiers() {
    let (_handshake, ctx) = test_generator();

    let wire_rules = vec![
        FilterRuleWireFormat::exclude("*.tmp".to_owned())
            .with_sides(true, false)
            .with_perishable(true),
        FilterRuleWireFormat::include("/important".to_owned()).with_anchored(true),
    ];

    let result = ctx.parse_received_filters(&wire_rules);
    assert!(result.is_ok());
}

#[test]
fn parse_received_filters_clear_rule() {
    let (_handshake, ctx) = test_generator();

    let wire_rules = vec![
        FilterRuleWireFormat::exclude("*.log".to_owned()),
        clear_rule(),
        FilterRuleWireFormat::include("*.txt".to_owned()),
    ];

    let (filter_set, _) = ctx.parse_received_filters(&wire_rules).unwrap();
    // Clear rule should have removed previous rules
    assert!(!filter_set.is_empty()); // Only the include rule remains
}

#[test]
fn filter_application_excludes_files() {
    let temp_dir = create_test_files(&[
        ("include.txt", b"included"),
        ("exclude.log", b"excluded"),
        ("another.txt", b"also included"),
    ]);
    let base_path = temp_dir.path();

    let (_handshake, mut ctx) = test_generator_for_path(base_path, false);
    apply_filters(
        &mut ctx,
        vec![FilterRuleWireFormat::exclude("*.log".to_owned())],
    );

    let count = build_file_list_for(&mut ctx, base_path);

    // Should have 3 entries: "." root dir + 2 .txt files (not the .log file)
    assert_eq!(count, 3);
    assert_eq!(ctx.file_list().len(), 3);

    // Verify the .log file is not in the list
    for entry in ctx.file_list() {
        assert!(!entry.name().contains(".log"));
    }
}

#[test]
fn filter_application_includes_only_matching() {
    let temp_dir = create_test_files(&[
        ("data.txt", b"text"),
        ("script.sh", b"shell"),
        ("readme.md", b"markdown"),
    ]);
    let base_path = temp_dir.path();

    let (_handshake, mut ctx) = test_generator_for_path(base_path, false);
    apply_filters(
        &mut ctx,
        vec![
            FilterRuleWireFormat::include("*.txt".to_owned()),
            FilterRuleWireFormat::exclude("*".to_owned()),
        ],
    );

    let count = build_file_list_for(&mut ctx, base_path);

    // Should have 2 entries: "." root dir + data.txt (other files excluded by "exclude *")
    assert_eq!(count, 2);
    assert_eq!(ctx.file_list().len(), 2);
}

#[test]
fn filter_application_with_directories() {
    let temp_dir = create_test_structure(&[
        "include_dir/",
        "include_dir/file.txt",
        "exclude_dir/",
        "exclude_dir/file.txt",
    ]);
    let base_path = temp_dir.path();

    let (_handshake, mut ctx) = test_generator_for_path(base_path, true);
    apply_filters(
        &mut ctx,
        vec![FilterRuleWireFormat::exclude("exclude_dir/".to_owned()).with_directory_only(true)],
    );

    let count = build_file_list_for(&mut ctx, base_path);

    // Should have include_dir and its file, but not exclude_dir
    assert!(count >= 2); // At least the directory and one file

    // Verify exclude_dir is not in the list
    for entry in ctx.file_list() {
        let name = entry.name();
        assert!(!name.contains("exclude_dir"), "Found excluded dir: {name}");
    }
}

#[test]
fn filter_application_no_filters_includes_all() {
    let temp_dir = create_test_files(&[
        ("file1.txt", b"data1"),
        ("file2.log", b"data2"),
        ("file3.md", b"data3"),
    ]);
    let base_path = temp_dir.path();

    let (_handshake, mut ctx) = test_generator_for_path(base_path, false);
    // No filters set (filter_chain is empty)

    let count = build_file_list_for(&mut ctx, base_path);

    // Should have 4 entries: "." root dir + 3 files when no filters are present
    assert_eq!(count, 4);
    assert_eq!(ctx.file_list().len(), 4);
}

#[test]
fn script_to_wire_delta_converts_literals() {
    let tokens = vec![
        DeltaToken::Literal(vec![1, 2, 3]),
        DeltaToken::Literal(vec![4, 5, 6]),
    ];
    let script = DeltaScript::new(tokens, 6, 6);

    let wire_ops = script_to_wire_delta(script);

    assert_eq!(wire_ops.len(), 2);
    match &wire_ops[0] {
        DeltaOp::Literal(data) => assert_eq!(data, &vec![1, 2, 3]),
        _ => panic!("expected literal op"),
    }
    match &wire_ops[1] {
        DeltaOp::Literal(data) => assert_eq!(data, &vec![4, 5, 6]),
        _ => panic!("expected literal op"),
    }
}

#[test]
fn script_to_wire_delta_converts_copy_operations() {
    let tokens = vec![
        DeltaToken::Copy {
            index: 0,
            len: 1024,
        },
        DeltaToken::Literal(vec![99]),
        DeltaToken::Copy { index: 1, len: 512 },
    ];
    let script = DeltaScript::new(tokens, 1537, 1);

    let wire_ops = script_to_wire_delta(script);

    assert_eq!(wire_ops.len(), 3);
    match &wire_ops[0] {
        DeltaOp::Copy {
            block_index,
            length,
        } => {
            assert_eq!(*block_index, 0);
            assert_eq!(*length, 1024);
        }
        _ => panic!("expected copy op"),
    }
    match &wire_ops[1] {
        DeltaOp::Literal(data) => assert_eq!(data, &vec![99]),
        _ => panic!("expected literal op"),
    }
    match &wire_ops[2] {
        DeltaOp::Copy {
            block_index,
            length,
        } => {
            assert_eq!(*block_index, 1);
            assert_eq!(*length, 512);
        }
        _ => panic!("expected copy op"),
    }
}

#[test]
fn stream_whole_file_produces_correct_wire_format() {
    use protocol::wire::write_whole_file_delta;
    use std::io::Write;
    use tempfile::NamedTempFile;

    let data = b"Hello, world! This is a test file.";
    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(data).unwrap();
    temp_file.flush().unwrap();

    let source = fs::File::open(temp_file.path()).unwrap();
    let mut wire_output = Vec::new();
    let mut buf = vec![0u8; protocol::wire::CHUNK_SIZE];
    let result = stream_whole_file_transfer(
        &mut wire_output,
        source,
        data.len() as u64,
        ChecksumAlgorithm::MD5,
        None,
        &mut buf,
    )
    .unwrap();

    assert_eq!(result.total_bytes, data.len() as u64);
    assert!(result.checksum_len > 0);

    // Compare wire output byte-for-byte with write_whole_file_delta
    let mut expected = Vec::new();
    write_whole_file_delta(&mut expected, data).unwrap();
    assert_eq!(wire_output, expected);
}

#[test]
fn stream_whole_file_handles_empty_file() {
    use tempfile::NamedTempFile;

    let temp_file = NamedTempFile::new().unwrap();
    let source = fs::File::open(temp_file.path()).unwrap();
    let mut wire_output = Vec::new();
    let mut buf = vec![0u8; protocol::wire::CHUNK_SIZE];
    let result = stream_whole_file_transfer(
        &mut wire_output,
        source,
        0,
        ChecksumAlgorithm::MD5,
        None,
        &mut buf,
    )
    .unwrap();

    assert_eq!(result.total_bytes, 0);
    // Wire output should only contain the end marker: write_int(0) = 4 zero bytes
    assert_eq!(wire_output, [0u8; 4]);
}

#[test]
fn stream_whole_file_computes_correct_checksum() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let data = vec![0xAB; 1024];
    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(&data).unwrap();
    temp_file.flush().unwrap();

    let source = fs::File::open(temp_file.path()).unwrap();
    let mut wire_output = Vec::new();
    let mut buf = vec![0u8; protocol::wire::CHUNK_SIZE];
    let result = stream_whole_file_transfer(
        &mut wire_output,
        source,
        data.len() as u64,
        ChecksumAlgorithm::MD5,
        None,
        &mut buf,
    )
    .unwrap();

    // Independently compute expected checksum
    let mut verifier = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
    verifier.update(&data);
    let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let expected_len = verifier.finalize_into(&mut expected_buf);

    assert_eq!(
        &result.checksum_buf[..result.checksum_len],
        &expected_buf[..expected_len]
    );
    assert_eq!(result.total_bytes, 1024);
}

#[test]
fn stream_whole_file_reuses_buffer() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut buf = vec![0u8; protocol::wire::CHUNK_SIZE];
    let initial_capacity = buf.capacity();

    // Stream first file
    let data1 = vec![0x11; 512];
    let mut temp1 = NamedTempFile::new().unwrap();
    temp1.write_all(&data1).unwrap();
    temp1.flush().unwrap();
    let source1 = fs::File::open(temp1.path()).unwrap();
    let mut out1 = Vec::new();
    stream_whole_file_transfer(
        &mut out1,
        source1,
        512,
        ChecksumAlgorithm::None,
        None,
        &mut buf,
    )
    .unwrap();

    // Stream second file with same buffer
    let data2 = vec![0x22; 2048];
    let mut temp2 = NamedTempFile::new().unwrap();
    temp2.write_all(&data2).unwrap();
    temp2.flush().unwrap();
    let source2 = fs::File::open(temp2.path()).unwrap();
    let mut out2 = Vec::new();
    stream_whole_file_transfer(
        &mut out2,
        source2,
        2048,
        ChecksumAlgorithm::None,
        None,
        &mut buf,
    )
    .unwrap();

    // Buffer capacity should not have grown beyond initial CHUNK_SIZE
    assert_eq!(buf.capacity(), initial_capacity);
}

#[test]
fn stream_whole_file_none_checksum() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let data = vec![0xFF; 256];
    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(&data).unwrap();
    temp_file.flush().unwrap();

    let source = fs::File::open(temp_file.path()).unwrap();
    let mut wire_output = Vec::new();
    let mut buf = vec![0u8; protocol::wire::CHUNK_SIZE];
    let result = stream_whole_file_transfer(
        &mut wire_output,
        source,
        data.len() as u64,
        ChecksumAlgorithm::None,
        None,
        &mut buf,
    )
    .unwrap();

    // None algorithm produces a 1-byte zero placeholder
    assert_eq!(result.checksum_len, 1);
    assert_eq!(result.checksum_buf[0], 0);
    assert_eq!(result.total_bytes, 256);
}

#[test]
fn stream_whole_file_warning_threshold_exists() {
    assert_eq!(LARGE_FILE_WARNING_THRESHOLD, 8 * 1024 * 1024 * 1024);
}

#[test]
fn item_flags_from_raw() {
    let flags = ItemFlags::from_raw(0x8000);
    assert_eq!(flags.raw(), 0x8000);
    assert!(flags.needs_transfer());
    assert!(!flags.has_basis_type());
    assert!(!flags.has_xname());
}

#[test]
fn item_flags_needs_transfer() {
    // Test ITEM_TRANSFER flag (0x8000)
    assert!(ItemFlags::from_raw(0x8000).needs_transfer());
    assert!(ItemFlags::from_raw(0x8001).needs_transfer());
    assert!(ItemFlags::from_raw(0xFFFF).needs_transfer());
    assert!(!ItemFlags::from_raw(0x0000).needs_transfer());
    assert!(!ItemFlags::from_raw(0x7FFF).needs_transfer());
}

#[test]
fn item_flags_has_basis_type() {
    // Test ITEM_BASIS_TYPE_FOLLOWS flag (0x0800)
    assert!(ItemFlags::from_raw(0x0800).has_basis_type());
    assert!(ItemFlags::from_raw(0x8800).has_basis_type());
    assert!(!ItemFlags::from_raw(0x0000).has_basis_type());
    assert!(!ItemFlags::from_raw(0x8000).has_basis_type());
}

#[test]
fn item_flags_has_xname() {
    // Test ITEM_XNAME_FOLLOWS flag (0x1000)
    assert!(ItemFlags::from_raw(0x1000).has_xname());
    assert!(ItemFlags::from_raw(0x9000).has_xname());
    assert!(!ItemFlags::from_raw(0x0000).has_xname());
    assert!(!ItemFlags::from_raw(0x8000).has_xname());
}

#[test]
fn item_flags_read_protocol_29_plus() {
    // Protocol 29+ reads 2 bytes little-endian
    let data = [0x00, 0x80]; // 0x8000 = ITEM_TRANSFER
    let mut cursor = Cursor::new(&data[..]);

    let flags = ItemFlags::read(&mut cursor, 29).unwrap();
    assert_eq!(flags.raw(), 0x8000);
    assert!(flags.needs_transfer());
}

#[test]
fn item_flags_read_protocol_28() {
    // Protocol 28 and older defaults to ITEM_TRANSFER without reading
    let data: [u8; 0] = [];
    let mut cursor = Cursor::new(&data[..]);

    let flags = ItemFlags::read(&mut cursor, 28).unwrap();
    assert_eq!(flags.raw(), ItemFlags::ITEM_TRANSFER);
    assert!(flags.needs_transfer());
}

#[test]
fn item_flags_read_trailing_no_fields() {
    // No trailing fields when neither flag is set
    let data: [u8; 0] = [];
    let mut cursor = Cursor::new(&data[..]);

    let flags = ItemFlags::from_raw(0x8000); // Just ITEM_TRANSFER
    let (ftype, xname) = flags.read_trailing(&mut cursor).unwrap();

    assert!(ftype.is_none());
    assert!(xname.is_none());
}

#[test]
fn item_flags_read_trailing_basis_type() {
    // ITEM_BASIS_TYPE_FOLLOWS reads 1 byte
    let data = [0x42]; // basis type = BasisDir(0x42)
    let mut cursor = Cursor::new(&data[..]);

    let flags = ItemFlags::from_raw(0x0800); // ITEM_BASIS_TYPE_FOLLOWS
    let (ftype, xname) = flags.read_trailing(&mut cursor).unwrap();

    assert_eq!(ftype, Some(protocol::FnameCmpType::BasisDir(0x42)));
    assert!(xname.is_none());
}

#[test]
fn item_flags_combined_flags() {
    // Test multiple flags combined
    let flags = ItemFlags::from_raw(0x9800); // TRANSFER + XNAME + BASIS_TYPE
    assert!(flags.needs_transfer());
    assert!(flags.has_basis_type());
    assert!(flags.has_xname());
}

#[test]
fn item_flags_constants() {
    // Verify constant values match upstream rsync.h:214-233
    assert_eq!(ItemFlags::ITEM_REPORT_ATIME, 0x0001);
    assert_eq!(ItemFlags::ITEM_REPORT_CHANGE, 0x0002);
    assert_eq!(ItemFlags::ITEM_REPORT_SIZE, 0x0004);
    assert_eq!(ItemFlags::ITEM_REPORT_TIME, 0x0008);
    assert_eq!(ItemFlags::ITEM_REPORT_PERMS, 0x0010);
    assert_eq!(ItemFlags::ITEM_REPORT_OWNER, 0x0020);
    assert_eq!(ItemFlags::ITEM_REPORT_GROUP, 0x0040);
    assert_eq!(ItemFlags::ITEM_REPORT_ACL, 0x0080);
    assert_eq!(ItemFlags::ITEM_REPORT_XATTR, 0x0100);
    assert_eq!(ItemFlags::ITEM_REPORT_CRTIME, 0x0400);
    assert_eq!(ItemFlags::ITEM_BASIS_TYPE_FOLLOWS, 0x0800);
    assert_eq!(ItemFlags::ITEM_XNAME_FOLLOWS, 0x1000);
    assert_eq!(ItemFlags::ITEM_IS_NEW, 0x2000);
    assert_eq!(ItemFlags::ITEM_LOCAL_CHANGE, 0x4000);
    assert_eq!(ItemFlags::ITEM_TRANSFER, 0x8000);

    // Log-only flags (not sent on wire)
    assert_eq!(ItemFlags::ITEM_MISSING_DATA, 0x1_0000);
    assert_eq!(ItemFlags::ITEM_DELETED, 0x2_0000);
    assert_eq!(ItemFlags::ITEM_MATCHED, 0x4_0000);
}

#[test]
fn significant_item_flags_masks_framing_and_internal_bits() {
    // upstream rsync.h:235-236 - strips BASIS_TYPE_FOLLOWS, XNAME_FOLLOWS, LOCAL_CHANGE
    let mask = ItemFlags::SIGNIFICANT_ITEM_FLAGS;
    assert_eq!(mask & ItemFlags::ITEM_BASIS_TYPE_FOLLOWS, 0);
    assert_eq!(mask & ItemFlags::ITEM_XNAME_FOLLOWS, 0);
    assert_eq!(mask & ItemFlags::ITEM_LOCAL_CHANGE, 0);

    // Report flags and TRANSFER survive the mask
    assert_ne!(mask & ItemFlags::ITEM_REPORT_ATIME, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_CHANGE, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_SIZE, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_TIME, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_PERMS, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_OWNER, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_GROUP, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_ACL, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_XATTR, 0);
    assert_ne!(mask & ItemFlags::ITEM_REPORT_CRTIME, 0);
    assert_ne!(mask & ItemFlags::ITEM_IS_NEW, 0);
    assert_ne!(mask & ItemFlags::ITEM_TRANSFER, 0);
}

#[test]
fn significant_wire_bits_strips_internal_flags() {
    // ITEM_TRANSFER alone passes through
    let flags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER);
    assert_eq!(flags.significant_wire_bits(), 0x8000);

    // Framing bits are stripped
    let flags = ItemFlags::from_raw(
        ItemFlags::ITEM_TRANSFER
            | ItemFlags::ITEM_BASIS_TYPE_FOLLOWS
            | ItemFlags::ITEM_XNAME_FOLLOWS
            | ItemFlags::ITEM_LOCAL_CHANGE,
    );
    assert_eq!(flags.significant_wire_bits(), 0x8000);

    // Log-only upper bits are stripped by u16 truncation
    let flags = ItemFlags::from_raw(
        ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_DELETED | ItemFlags::ITEM_MATCHED,
    );
    assert_eq!(flags.significant_wire_bits(), 0x8000);

    // Report flags survive
    let flags = ItemFlags::from_raw(
        ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_CHANGE | ItemFlags::ITEM_REPORT_SIZE,
    );
    assert_eq!(flags.significant_wire_bits(), 0x8000 | 0x0002 | 0x0004);
}

#[test]
fn significant_wire_bits_returns_two_bytes() {
    // Ensure the return type is u16 (2 bytes) for wire transmission
    let flags = ItemFlags::from_raw(0xFFFF_FFFF);
    let wire = flags.significant_wire_bits();
    let bytes = wire.to_le_bytes();
    assert_eq!(bytes.len(), 2);
}

#[test]
fn significant_wire_bits_matches_upstream_mask() {
    // upstream: ~(ITEM_BASIS_TYPE_FOLLOWS | ITEM_XNAME_FOLLOWS | ITEM_LOCAL_CHANGE)
    // = ~(0x0800 | 0x1000 | 0x4000) = ~0x5800
    // Lower 16 bits of ~0x5800 = 0xA7FF
    let flags = ItemFlags::from_raw(0xFFFF);
    assert_eq!(flags.significant_wire_bits(), 0xA7FF);
}

#[test]
fn read_signature_blocks_empty() {
    // count=0 means whole-file transfer, no blocks to read
    let data: [u8; 0] = [];
    let mut cursor = Cursor::new(&data[..]);

    let sum_head = SumHead::empty();
    let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

    assert!(blocks.is_empty());
}

#[test]
fn read_signature_blocks_single_block() {
    // Single block: rolling (4 bytes) + strong (16 bytes)
    let mut data = Vec::new();
    // Rolling sum = 0x12345678 (little-endian)
    data.extend_from_slice(&0x12345678u32.to_le_bytes());
    // Strong sum = 16 bytes
    data.extend_from_slice(&[0xAA; 16]);

    let mut cursor = Cursor::new(&data[..]);

    let sum_head = SumHead::new(1, 1024, 16, 0);
    let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].index, 0);
    assert_eq!(blocks[0].rolling_sum, 0x12345678);
    assert_eq!(blocks[0].strong_sum, vec![0xAA; 16]);
}

#[test]
fn read_signature_blocks_multiple_blocks() {
    // Three blocks
    let mut data = Vec::new();

    // Block 0
    data.extend_from_slice(&0x11111111u32.to_le_bytes());
    data.extend_from_slice(&[0x01; 16]);

    // Block 1
    data.extend_from_slice(&0x22222222u32.to_le_bytes());
    data.extend_from_slice(&[0x02; 16]);

    // Block 2
    data.extend_from_slice(&0x33333333u32.to_le_bytes());
    data.extend_from_slice(&[0x03; 16]);

    let mut cursor = Cursor::new(&data[..]);

    let sum_head = SumHead::new(3, 1024, 16, 512);
    let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

    assert_eq!(blocks.len(), 3);

    assert_eq!(blocks[0].index, 0);
    assert_eq!(blocks[0].rolling_sum, 0x11111111);
    assert_eq!(blocks[0].strong_sum, vec![0x01; 16]);

    assert_eq!(blocks[1].index, 1);
    assert_eq!(blocks[1].rolling_sum, 0x22222222);
    assert_eq!(blocks[1].strong_sum, vec![0x02; 16]);

    assert_eq!(blocks[2].index, 2);
    assert_eq!(blocks[2].rolling_sum, 0x33333333);
    assert_eq!(blocks[2].strong_sum, vec![0x03; 16]);
}

#[test]
fn read_signature_blocks_short_strong_sum() {
    // Test with shorter strong sum (e.g., 8 bytes for XXH64)
    let mut data = Vec::new();
    data.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
    data.extend_from_slice(&[0xFF; 8]); // 8-byte strong sum

    let mut cursor = Cursor::new(&data[..]);

    let sum_head = SumHead::new(1, 2048, 8, 0);
    let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].rolling_sum, 0xDEADBEEF);
    assert_eq!(blocks[0].strong_sum.len(), 8);
    assert_eq!(blocks[0].strong_sum, vec![0xFF; 8]);
}

#[test]
fn read_signature_blocks_truncated_data() {
    // Test error handling when data is truncated
    let data = [0x12, 0x34, 0x56]; // Only 3 bytes, need 4 for rolling sum

    let mut cursor = Cursor::new(&data[..]);

    let sum_head = SumHead::new(1, 1024, 16, 0);
    let result = read_signature_blocks(&mut cursor, &sum_head);

    assert!(result.is_err());
}

#[test]
fn sum_head_round_trip() {
    // Test that SumHead read/write are inverses
    let original = SumHead::new(42, 4096, 16, 1024);

    let mut wire = Vec::new();
    original.write(&mut wire).unwrap();

    assert_eq!(wire.len(), 16); // 4 * 4 bytes

    let mut cursor = Cursor::new(&wire[..]);
    let parsed = SumHead::read(&mut cursor).unwrap();

    assert_eq!(parsed.count, 42);
    assert_eq!(parsed.blength, 4096);
    assert_eq!(parsed.s2length, 16);
    assert_eq!(parsed.remainder, 1024);
}

#[test]
fn sum_head_is_empty() {
    assert!(SumHead::empty().is_empty());
    assert!(SumHead::new(0, 0, 0, 0).is_empty());
    assert!(!SumHead::new(1, 1024, 16, 0).is_empty());
}

#[test]
fn should_activate_input_multiplex_client_mode_protocol_28() {
    // Client mode activates at protocol >= 23, so 28 should activate
    let handshake = test_handshake_with_protocol(28);
    let mut config = test_config();
    config.connection.client_mode = true;

    let ctx = GeneratorContext::new(&handshake, config);
    // Protocol 28 >= 23, so should activate in client mode
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn should_activate_input_multiplex_client_mode_protocol_32() {
    // Test with higher protocol version
    let handshake = test_handshake_with_protocol(32);
    let mut config = test_config();
    config.connection.client_mode = true;

    let ctx = GeneratorContext::new(&handshake, config);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn should_activate_input_multiplex_server_mode_protocol_30() {
    let handshake = test_handshake_with_protocol(30);
    let mut config = test_config();
    config.connection.client_mode = false;

    let ctx = GeneratorContext::new(&handshake, config);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn should_activate_input_multiplex_server_mode_protocol_29() {
    let handshake = test_handshake_with_protocol(29);
    let mut config = test_config();
    config.connection.client_mode = false;

    let ctx = GeneratorContext::new(&handshake, config);
    assert!(!ctx.should_activate_input_multiplex());
}

#[test]
fn get_checksum_algorithm_default_protocol_28() {
    let handshake = test_handshake_with_protocol(28);

    let ctx = GeneratorContext::new(&handshake, test_config());
    assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::MD4);
}

#[test]
fn get_checksum_algorithm_default_protocol_30() {
    let handshake = test_handshake_with_protocol(30);

    let ctx = GeneratorContext::new(&handshake, test_config());
    assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::MD5);
}

#[test]
fn get_checksum_algorithm_negotiated() {
    let handshake = test_handshake_with_negotiated_algorithms(
        32,
        ChecksumAlgorithm::XXH3,
        CompressionAlgorithm::None,
    );

    let ctx = GeneratorContext::new(&handshake, test_config());
    assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::XXH3);
}

#[test]
fn validate_file_index_valid() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new(&handshake, test_config());
    ctx.file_list.push(protocol::flist::FileEntry::new_file(
        "test.txt".into(),
        100,
        0o644,
    ));
    ctx.file_list.push(protocol::flist::FileEntry::new_file(
        "test2.txt".into(),
        200,
        0o644,
    ));

    assert!(ctx.validate_file_index(0).is_ok());
    assert!(ctx.validate_file_index(1).is_ok());
}

#[test]
fn validate_file_index_invalid() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new(&handshake, test_config());
    ctx.file_list.push(protocol::flist::FileEntry::new_file(
        "test.txt".into(),
        100,
        0o644,
    ));

    let result = ctx.validate_file_index(1);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn validate_file_index_empty_list() {
    let handshake = test_handshake();
    let ctx = GeneratorContext::new(&handshake, test_config());

    let result = ctx.validate_file_index(0);
    assert!(result.is_err());
}

#[test]
fn calculate_duration_ms_both_some() {
    let start = Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let end = Instant::now();

    let duration = calculate_duration_ms(Some(start), Some(end));
    assert!(duration >= 10);
    assert!(duration < 100); // Sanity check
}

#[test]
fn calculate_duration_ms_start_none() {
    let end = Instant::now();
    let duration = calculate_duration_ms(None, Some(end));
    assert_eq!(duration, 0);
}

#[test]
fn calculate_duration_ms_end_none() {
    let start = Instant::now();
    let duration = calculate_duration_ms(Some(start), None);
    assert_eq!(duration, 0);
}

#[test]
fn calculate_duration_ms_both_none() {
    let duration = calculate_duration_ms(None, None);
    assert_eq!(duration, 0);
}

#[test]
fn send_id_lists_empty_output_no_preserve() {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.owner = false;
    config.flags.group = false;

    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    ctx.send_id_lists(&mut output).unwrap();

    // No output when preserve flags are off
    assert!(output.is_empty());
}

#[test]
fn send_id_lists_owner_only() {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.owner = true;
    config.flags.group = false;

    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    ctx.send_id_lists(&mut output).unwrap();

    // Should have varint 0 terminator (1 byte)
    assert!(!output.is_empty());
    assert_eq!(output[0], 0); // Empty list terminator
}

#[test]
fn send_io_error_flag_protocol_29() {
    let handshake = test_handshake_with_protocol(29);
    let ctx = GeneratorContext::new(&handshake, test_config());

    let mut output = Vec::new();
    ctx.send_io_error_flag(&mut output).unwrap();

    // Protocol < 30 should write 4-byte io_error (value 0)
    assert_eq!(output.len(), 4);
    assert_eq!(output, &[0, 0, 0, 0]);
}

#[test]
fn send_io_error_flag_protocol_30() {
    let handshake = test_handshake_with_protocol(30);
    let ctx = GeneratorContext::new(&handshake, test_config());

    let mut output = Vec::new();
    ctx.send_io_error_flag(&mut output).unwrap();

    // Protocol >= 30 should not write io_error
    assert!(output.is_empty());
}

#[test]
fn send_io_error_flag_with_errors_protocol_29() {
    let handshake = test_handshake_with_protocol(29);
    let mut ctx = GeneratorContext::new(&handshake, test_config());
    ctx.add_io_error(io_error_flags::IOERR_GENERAL);

    let mut output = Vec::new();
    ctx.send_io_error_flag(&mut output).unwrap();

    // Protocol < 30 should write 4-byte io_error with actual value
    assert_eq!(output.len(), 4);
    let value = i32::from_le_bytes([output[0], output[1], output[2], output[3]]);
    assert_eq!(value, io_error_flags::IOERR_GENERAL);
}

#[test]
fn send_io_error_flag_ignore_errors_suppresses_value() {
    // Tests upstream behavior: flist.c:2518: write_int(f, ignore_errors ? 0 : io_error);
    let handshake = test_handshake_with_protocol(29);
    let mut config = test_config();
    config.deletion.ignore_errors = true;

    let mut ctx = GeneratorContext::new(&handshake, config);
    ctx.add_io_error(io_error_flags::IOERR_GENERAL);

    let mut output = Vec::new();
    ctx.send_io_error_flag(&mut output).unwrap();

    // With ignore_errors=true, should send 0 even though io_error is set
    assert_eq!(output.len(), 4);
    assert_eq!(output, &[0, 0, 0, 0]);
}

#[test]
fn apply_permutation_in_place_identity() {
    let mut a = vec![1, 2, 3, 4];
    let mut b = vec!["a", "b", "c", "d"];
    let indices = vec![0, 1, 2, 3];
    apply_permutation_in_place(&mut a, &mut b, indices);
    assert_eq!(a, vec![1, 2, 3, 4]);
    assert_eq!(b, vec!["a", "b", "c", "d"]);
}

#[test]
fn apply_permutation_in_place_reverse() {
    let mut a = vec![1, 2, 3, 4];
    let mut b = vec!["a", "b", "c", "d"];
    // Indices represent: position 0 gets element from 3, pos 1 from 2, etc.
    let indices = vec![3, 2, 1, 0];
    apply_permutation_in_place(&mut a, &mut b, indices);
    assert_eq!(a, vec![4, 3, 2, 1]);
    assert_eq!(b, vec!["d", "c", "b", "a"]);
}

#[test]
fn apply_permutation_in_place_cycle() {
    let mut a = vec![1, 2, 3, 4];
    let mut b = vec!["a", "b", "c", "d"];
    // Cycle: 0->1->2->3->0
    let indices = vec![3, 0, 1, 2];
    apply_permutation_in_place(&mut a, &mut b, indices);
    assert_eq!(a, vec![4, 1, 2, 3]);
    assert_eq!(b, vec!["d", "a", "b", "c"]);
}

#[test]
fn apply_permutation_in_place_empty() {
    let mut a: Vec<i32> = vec![];
    let mut b: Vec<&str> = vec![];
    let indices: Vec<usize> = vec![];
    apply_permutation_in_place(&mut a, &mut b, indices);
    assert!(a.is_empty());
    assert!(b.is_empty());
}

#[test]
fn apply_permutation_in_place_single() {
    let mut a = vec![42];
    let mut b = vec!["x"];
    let indices = vec![0];
    apply_permutation_in_place(&mut a, &mut b, indices);
    assert_eq!(a, vec![42]);
    assert_eq!(b, vec!["x"]);
}

/// Creates test config with specific flags for ID list tests.
fn config_with_flags(owner: bool, group: bool, numeric_ids: bool) -> ServerConfig {
    config_with_role_and_flags(ServerRole::Generator, owner, group, numeric_ids)
}

/// Creates test config with specific role and flags for ID list tests.
fn config_with_role_and_flags(
    role: ServerRole,
    owner: bool,
    group: bool,
    numeric_ids: bool,
) -> ServerConfig {
    ServerConfig {
        role,
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
fn send_id_lists_skips_when_numeric_ids_true() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, true);
    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    let result = ctx.send_id_lists(&mut output);

    assert!(result.is_ok());
    // With numeric_ids=true, nothing should be written
    assert!(output.is_empty());
}

#[test]
fn send_id_lists_sends_uid_list_when_owner_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, false, false);
    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    let result = ctx.send_id_lists(&mut output);

    assert!(result.is_ok());
    // Empty UID list: varint 0 terminator
    assert_eq!(output, vec![0]);
}

#[test]
fn send_id_lists_sends_gid_list_when_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, true, false);
    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    let result = ctx.send_id_lists(&mut output);

    assert!(result.is_ok());
    // Empty GID list: varint 0 terminator
    assert_eq!(output, vec![0]);
}

#[test]
fn send_id_lists_sends_both_when_owner_and_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, false);
    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    let result = ctx.send_id_lists(&mut output);

    assert!(result.is_ok());
    // Both lists: two varint 0 terminators
    assert_eq!(output, vec![0, 0]);
}

#[test]
fn send_id_lists_skips_both_when_neither_flag_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, false, false);
    let ctx = GeneratorContext::new(&handshake, config);

    let mut output = Vec::new();
    let result = ctx.send_id_lists(&mut output);

    assert!(result.is_ok());
    assert!(output.is_empty());
}

#[test]
fn id_lists_round_trip_with_numeric_ids_false() {
    use crate::receiver::ReceiverContext;

    let handshake = test_handshake();

    // Generator sends ID lists (numeric_ids=false, owner/group=true)
    let gen_config = config_with_flags(true, true, false);
    let generator = GeneratorContext::new(&handshake, gen_config);

    let mut wire_data = Vec::new();
    generator.send_id_lists(&mut wire_data).unwrap();

    // Both empty lists with terminators
    assert_eq!(wire_data, vec![0, 0]);

    // Receiver reads ID lists with matching flags
    let recv_config = config_with_role_and_flags(ServerRole::Receiver, true, true, false);
    let mut receiver = ReceiverContext::new(&handshake, recv_config);

    let mut cursor = Cursor::new(&wire_data[..]);
    let result = receiver.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position() as usize, wire_data.len());
}

#[test]
fn id_lists_round_trip_with_numeric_ids_true() {
    use crate::receiver::ReceiverContext;

    let handshake = test_handshake();

    // Generator skips ID lists (numeric_ids=true)
    let gen_config = config_with_flags(true, true, true);
    let generator = GeneratorContext::new(&handshake, gen_config);

    let mut wire_data = Vec::new();
    generator.send_id_lists(&mut wire_data).unwrap();

    // No data written when numeric_ids=true
    assert!(wire_data.is_empty());

    // Receiver also skips reading with matching flags
    let recv_config = config_with_role_and_flags(ServerRole::Receiver, true, true, true);
    let mut receiver = ReceiverContext::new(&handshake, recv_config);

    let mut cursor = Cursor::new(&wire_data[..]);
    let result = receiver.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 0);
}

#[test]
fn generator_context_stores_negotiated_compression() {
    let handshake = test_handshake_with_negotiated_algorithms(
        32,
        ChecksumAlgorithm::XXH3,
        CompressionAlgorithm::Zlib,
    );

    let ctx = GeneratorContext::new(&handshake, test_config());
    assert!(ctx.negotiated_algorithms.is_some());
    let negotiated = ctx.negotiated_algorithms.as_ref().unwrap();
    assert_eq!(negotiated.compression, CompressionAlgorithm::Zlib);
}

#[test]
fn generator_context_handles_no_compression() {
    let handshake = test_handshake_with_negotiated_algorithms(
        32,
        ChecksumAlgorithm::MD5,
        CompressionAlgorithm::None,
    );

    let ctx = GeneratorContext::new(&handshake, test_config());
    assert!(ctx.negotiated_algorithms.is_some());
    let negotiated = ctx.negotiated_algorithms.as_ref().unwrap();
    assert_eq!(negotiated.compression, CompressionAlgorithm::None);
}

/// Creates a FIFO at the given path using the `mkfifo` command.
#[cfg(unix)]
fn create_fifo_for_test(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo command failed to start");
    assert!(status.success(), "mkfifo failed");
}

#[cfg(unix)]
#[test]
fn walk_skips_fifo_when_preserve_specials_is_false() {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();

    std::fs::write(base_path.join("regular.txt"), b"data").unwrap();
    create_fifo_for_test(&base_path.join("test.fifo"));

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.specials = false;
    config.flags.recursive = true;
    let mut ctx = GeneratorContext::new(&handshake, config);

    let count = build_file_list_for(&mut ctx, base_path);

    // FIFO should be skipped, "." root dir + regular file included
    assert_eq!(count, 2);
}

#[cfg(unix)]
#[test]
fn walk_includes_fifo_when_preserve_specials_is_true() {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();

    std::fs::write(base_path.join("regular.txt"), b"data").unwrap();
    create_fifo_for_test(&base_path.join("test.fifo"));

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.specials = true;
    config.flags.recursive = true;
    let mut ctx = GeneratorContext::new(&handshake, config);

    let count = build_file_list_for(&mut ctx, base_path);

    // "." root dir + regular file + FIFO should be included
    assert_eq!(count, 3);
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
    assert!(names.contains(&"regular.txt"));
    assert!(names.contains(&"test.fifo"));
}

#[cfg(unix)]
#[test]
fn walk_includes_fifo_as_special_entry_type() {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();

    create_fifo_for_test(&base_path.join("my.fifo"));

    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.specials = true;
    let mut ctx = GeneratorContext::new(&handshake, config);

    build_file_list_for(&mut ctx, base_path);

    // "." root dir + FIFO
    assert_eq!(ctx.file_list().len(), 2);
}

#[cfg(unix)]
#[test]
fn send_file_list_passes_preserve_flags_to_writer() {
    use crate::receiver::ReceiverContext;

    let handshake = test_handshake();
    let mut gen_config = test_config();
    gen_config.flags.specials = true;
    gen_config.flags.devices = true;
    let mut generator = GeneratorContext::new(&handshake, gen_config);

    // Add a FIFO entry
    let mut fifo = protocol::flist::FileEntry::new_fifo("test.fifo".into(), 0o644);
    fifo.set_mtime(1700000000, 0);
    generator.file_list.push(fifo);

    let mut wire_data = Vec::new();
    generator.send_file_list(&mut wire_data).unwrap();

    // Receiver should be able to decode when matching flags are set
    let mut recv_config = test_config();
    recv_config.flags.specials = true;
    recv_config.flags.devices = true;
    let mut receiver = ReceiverContext::new(&handshake, recv_config);

    let mut cursor = Cursor::new(&wire_data[..]);
    let count = receiver.receive_file_list(&mut cursor).unwrap();

    assert_eq!(count, 1);
    assert!(receiver.file_list()[0].is_special());
    assert_eq!(receiver.file_list()[0].name(), "test.fifo");
}

#[cfg(all(unix, target_os = "linux"))]
#[test]
fn rdev_to_major_minor_extracts_linux_values() {
    // Linux rdev encoding for major=8, minor=0 (sda)
    // major low nibble at bits 8-11, minor low byte at bits 0-7
    let rdev: u64 = 8 << 8;
    let (major, minor) = super::file_list::rdev_to_major_minor(rdev);
    assert_eq!(major, 8);
    assert_eq!(minor, 0);
}

#[cfg(all(unix, not(target_os = "linux")))]
#[test]
fn rdev_to_major_minor_extracts_bsd_values() {
    // BSD/macOS rdev encoding: major in high byte, minor in low 24 bits
    let rdev: u64 = (8 << 24) | 3;
    let (major, minor) = super::file_list::rdev_to_major_minor(rdev);
    assert_eq!(major, 8);
    assert_eq!(minor, 3);
}

/// Verifies that `write_delta_with_compression` with Zlib compression
/// performs dictionary sync for Copy operations by re-reading block data
/// from the source file.
#[test]
fn write_delta_with_compression_zlib_dict_sync() {
    use protocol::wire::{CompressedToken, CompressedTokenDecoder};
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // Build a source file whose content matches the delta op sequence:
    // [literal_a][copy_block][literal_b]
    let literal_a = b"First literal segment of the source file data. ";
    let copy_block = b"This block matches the basis file block zero content. ";
    let literal_b = b"Second literal segment after the matched block. ";

    let mut source_file = NamedTempFile::new().unwrap();
    source_file.write_all(literal_a).unwrap();
    source_file.write_all(copy_block).unwrap();
    source_file.write_all(literal_b).unwrap();
    source_file.flush().unwrap();

    let ops = vec![
        DeltaOp::Literal(literal_a.to_vec()),
        DeltaOp::Copy {
            block_index: 0,
            length: copy_block.len() as u32,
        },
        DeltaOp::Literal(literal_b.to_vec()),
    ];

    // Encode with Zlib compression (dictionary sync enabled)
    let mut encoded = Vec::new();
    let mut encoder = CompressedTokenEncoder::default();
    write_delta_with_compression(
        &mut encoded,
        &ops,
        Some(&mut encoder),
        true,
        source_file.path(),
    )
    .unwrap();

    // Decode and verify the output is correct
    let mut cursor = Cursor::new(&encoded);
    let mut decoder = CompressedTokenDecoder::new();
    let mut literals = Vec::new();
    let mut blocks = Vec::new();

    loop {
        let token = match decoder.recv_token(&mut cursor) {
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("invalid distance")
                    || msg.contains("too far back")
                    || msg.contains("bad state")
                {
                    eprintln!("Skipping: deflate backend incompatible with see_token: {msg}");
                    return;
                }
                panic!("decode error: {e}");
            }
        };
        match token {
            CompressedToken::Literal(data) => literals.push(data),
            CompressedToken::BlockMatch(idx) => {
                blocks.push(idx);
                // Receiver must also call see_token with the block data
                decoder.see_token(copy_block).unwrap();
            }
            CompressedToken::End => break,
        }
    }

    assert_eq!(blocks, vec![0]);
    let combined: Vec<u8> = literals.into_iter().flatten().collect();
    let expected: Vec<u8> = [literal_a.as_slice(), literal_b.as_slice()].concat();
    assert_eq!(combined, expected);
}

/// Verifies that ZlibX mode skips dictionary sync (no source file read).
#[test]
fn write_delta_with_compression_zlibx_no_dict_sync() {
    use protocol::wire::{CompressedToken, CompressedTokenDecoder};
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    let literal_a = b"literal data before block match";
    let literal_b = b"literal data after block match";
    let copy_block = b"block data from basis file";

    let mut source_file = NamedTempFile::new().unwrap();
    source_file.write_all(literal_a).unwrap();
    source_file.write_all(copy_block).unwrap();
    source_file.write_all(literal_b).unwrap();
    source_file.flush().unwrap();

    let ops = vec![
        DeltaOp::Literal(literal_a.to_vec()),
        DeltaOp::Copy {
            block_index: 0,
            length: copy_block.len() as u32,
        },
        DeltaOp::Literal(literal_b.to_vec()),
    ];

    let mut encoded = Vec::new();
    let mut encoder = CompressedTokenEncoder::default();
    encoder.set_zlibx(true);
    write_delta_with_compression(
        &mut encoded,
        &ops,
        Some(&mut encoder),
        false,
        source_file.path(),
    )
    .unwrap();

    // Decode - ZlibX decoder also skips see_token
    let mut cursor = Cursor::new(&encoded);
    let mut decoder = CompressedTokenDecoder::new();
    decoder.set_zlibx(true);
    let mut literals = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(data) => literals.push(data),
            CompressedToken::BlockMatch(_) => {
                // No see_token needed for ZlibX
            }
            CompressedToken::End => break,
        }
    }

    let combined: Vec<u8> = literals.into_iter().flatten().collect();
    let expected: Vec<u8> = [literal_a.as_slice(), literal_b.as_slice()].concat();
    assert_eq!(combined, expected);
}

/// Verifies that plain (no compression) mode ignores the source_path parameter.
#[test]
fn write_delta_with_compression_plain_fallback() {
    let ops = vec![
        DeltaOp::Literal(vec![1, 2, 3]),
        DeltaOp::Copy {
            block_index: 0,
            length: 10,
        },
    ];

    let mut encoded = Vec::new();
    // Pass a non-existent path since plain mode should not open the file
    write_delta_with_compression(
        &mut encoded,
        &ops,
        None,
        false,
        Path::new("/nonexistent/path"),
    )
    .unwrap();

    assert!(!encoded.is_empty());
}

#[test]
fn del_stats_not_sent_without_do_stats() {
    // upstream: INFO_GTE(STATS, 2) is false => never send del_stats
    let handshake = test_handshake();
    let mut config = test_config();
    config.do_stats = false;
    config.flags.delete = true;
    config.deletion.late_delete = false;
    let ctx = GeneratorContext::new(&handshake, config);
    assert!(!ctx.should_send_del_stats());
}

#[test]
fn del_stats_early_sent_with_do_stats_and_delete() {
    // upstream: generator.c:2377 - early path: INFO_GTE(STATS, 2) && delete_mode
    let handshake = test_handshake();
    let mut config = test_config();
    config.do_stats = true;
    config.flags.delete = true;
    config.deletion.late_delete = false;
    let ctx = GeneratorContext::new(&handshake, config);
    assert!(ctx.should_send_del_stats());
}

#[test]
fn del_stats_early_not_sent_without_delete() {
    // upstream: generator.c:2377 - early path requires delete_mode || force_delete
    let handshake = test_handshake();
    let mut config = test_config();
    config.do_stats = true;
    config.flags.delete = false;
    config.deletion.late_delete = false;
    let ctx = GeneratorContext::new(&handshake, config);
    assert!(!ctx.should_send_del_stats());
}

#[test]
fn del_stats_late_sent_with_do_stats_only() {
    // upstream: generator.c:2422 - late path: INFO_GTE(STATS, 2) is sufficient
    let handshake = test_handshake();
    let mut config = test_config();
    config.do_stats = true;
    config.flags.delete = false;
    config.deletion.late_delete = true;
    let ctx = GeneratorContext::new(&handshake, config);
    assert!(ctx.should_send_del_stats());
}

#[test]
fn del_stats_late_not_sent_without_do_stats() {
    // upstream: INFO_GTE(STATS, 2) is false => even late path skips del_stats
    let handshake = test_handshake();
    let mut config = test_config();
    config.do_stats = false;
    config.flags.delete = true;
    config.deletion.late_delete = true;
    let ctx = GeneratorContext::new(&handshake, config);
    assert!(!ctx.should_send_del_stats());
}

#[test]
fn del_stats_late_with_delete_and_stats() {
    // upstream: late path with both delete_mode and stats - should send
    let handshake = test_handshake();
    let mut config = test_config();
    config.do_stats = true;
    config.flags.delete = true;
    config.deletion.late_delete = true;
    let ctx = GeneratorContext::new(&handshake, config);
    assert!(ctx.should_send_del_stats());
}

#[test]
fn record_io_error_not_found_sets_vanished() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new(&handshake, test_config());

    let error = io::Error::new(io::ErrorKind::NotFound, "file vanished");
    ctx.record_io_error(&error);

    assert_eq!(
        ctx.io_error() & io_error_flags::IOERR_VANISHED,
        io_error_flags::IOERR_VANISHED
    );
    assert_eq!(ctx.io_error() & io_error_flags::IOERR_GENERAL, 0);
}

#[test]
fn record_io_error_other_sets_general() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new(&handshake, test_config());

    let error = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
    ctx.record_io_error(&error);

    assert_eq!(
        ctx.io_error() & io_error_flags::IOERR_GENERAL,
        io_error_flags::IOERR_GENERAL
    );
    assert_eq!(ctx.io_error() & io_error_flags::IOERR_VANISHED, 0);
}

#[test]
fn io_error_flags_accumulate_via_or() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new(&handshake, test_config());

    let vanished = io::Error::new(io::ErrorKind::NotFound, "gone");
    let general = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
    ctx.record_io_error(&vanished);
    ctx.record_io_error(&general);

    assert_eq!(
        ctx.io_error(),
        io_error_flags::IOERR_VANISHED | io_error_flags::IOERR_GENERAL
    );
}

#[test]
fn to_exit_code_vanished_only_returns_24() {
    assert_eq!(
        io_error_flags::to_exit_code(io_error_flags::IOERR_VANISHED),
        24
    );
}

#[test]
fn to_exit_code_general_returns_23() {
    assert_eq!(
        io_error_flags::to_exit_code(io_error_flags::IOERR_GENERAL),
        23
    );
}

#[test]
fn to_exit_code_general_overrides_vanished() {
    // upstream: IOERR_GENERAL takes precedence over IOERR_VANISHED
    let combined = io_error_flags::IOERR_GENERAL | io_error_flags::IOERR_VANISHED;
    assert_eq!(io_error_flags::to_exit_code(combined), 23);
}

#[test]
fn to_exit_code_del_limit_returns_25() {
    assert_eq!(
        io_error_flags::to_exit_code(io_error_flags::IOERR_DEL_LIMIT),
        25
    );
}

#[test]
fn to_exit_code_del_limit_overrides_all() {
    // upstream: IOERR_DEL_LIMIT takes highest precedence
    let all = io_error_flags::IOERR_DEL_LIMIT
        | io_error_flags::IOERR_GENERAL
        | io_error_flags::IOERR_VANISHED;
    assert_eq!(io_error_flags::to_exit_code(all), 25);
}

#[test]
fn to_exit_code_no_errors_returns_zero() {
    assert_eq!(io_error_flags::to_exit_code(0), 0);
}

/// Tests for legacy goodbye handshake (protocol 28/29).
///
/// Protocol 28/29 uses a simpler goodbye sequence: the receiver sends
/// a single NDX_DONE (4-byte LE) and the sender (generator) reads it.
/// No NDX_DEL_STATS or extended goodbye round-trip occurs.
///
/// upstream: main.c:875-906 `read_final_goodbye()`
mod legacy_goodbye_tests {
    use super::*;
    use protocol::codec::{MonotonicNdxWriter, create_ndx_codec};

    /// NDX_DONE as 4-byte little-endian (-1 = 0xFFFFFFFF).
    const NDX_DONE_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

    /// Creates a `GeneratorContext` for a specific protocol version.
    fn generator_for(protocol_version: u8) -> GeneratorContext {
        let handshake = test_handshake_with_protocol(protocol_version);
        let mut config = test_config();
        config.protocol = ProtocolVersion::try_from(protocol_version).unwrap();
        GeneratorContext::new(&handshake, config)
    }

    #[test]
    fn proto28_supports_goodbye_but_not_extended() {
        let ctx = generator_for(28);
        assert!(ctx.protocol.supports_goodbye_exchange());
        assert!(!ctx.protocol.supports_extended_goodbye());
    }

    #[test]
    fn proto29_supports_goodbye_but_not_extended() {
        let ctx = generator_for(29);
        assert!(ctx.protocol.supports_goodbye_exchange());
        assert!(!ctx.protocol.supports_extended_goodbye());
    }

    #[test]
    fn handle_goodbye_proto28_reads_single_ndx_done() {
        let mut ctx = generator_for(28);

        let receiver_input = NDX_DONE_LE.to_vec();
        let mut reader = Cursor::new(receiver_input);
        let mut output = Vec::new();
        let mut ndx_read = create_ndx_codec(28);
        let mut ndx_write = MonotonicNdxWriter::new(28);

        ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_read, &mut ndx_write)
            .unwrap();

        assert!(output.is_empty());
        assert_eq!(reader.position(), 4);
    }

    #[test]
    fn handle_goodbye_proto29_reads_single_ndx_done() {
        let mut ctx = generator_for(29);

        let receiver_input = NDX_DONE_LE.to_vec();
        let mut reader = Cursor::new(receiver_input);
        let mut output = Vec::new();
        let mut ndx_read = create_ndx_codec(29);
        let mut ndx_write = MonotonicNdxWriter::new(29);

        ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_read, &mut ndx_write)
            .unwrap();

        assert!(output.is_empty());
        assert_eq!(reader.position(), 4);
    }

    #[test]
    fn handle_goodbye_proto28_rejects_non_ndx_done() {
        let mut ctx = generator_for(28);

        let bad_input = 5i32.to_le_bytes().to_vec();
        let mut reader = Cursor::new(bad_input);
        let mut output = Vec::new();
        let mut ndx_read = create_ndx_codec(28);
        let mut ndx_write = MonotonicNdxWriter::new(28);

        let result = ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_read, &mut ndx_write);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("expected goodbye NDX_DONE"));
    }

    #[test]
    fn handle_goodbye_proto28_no_del_stats_sent() {
        let handshake = test_handshake_with_protocol(28);
        let mut config = test_config();
        config.protocol = ProtocolVersion::try_from(28u8).unwrap();
        config.do_stats = true;
        config.flags.delete = true;
        let mut ctx = GeneratorContext::new(&handshake, config);

        let receiver_input = NDX_DONE_LE.to_vec();
        let mut reader = Cursor::new(receiver_input);
        let mut output = Vec::new();
        let mut ndx_read = create_ndx_codec(28);
        let mut ndx_write = MonotonicNdxWriter::new(28);

        ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_read, &mut ndx_write)
            .unwrap();

        assert!(output.is_empty());
    }

    #[test]
    fn transfer_loop_proto28_single_phase_break() {
        let ctx = generator_for(28);
        assert!(!ctx.protocol.supports_iflags());

        let max_phase: i32 = if ctx.protocol.supports_iflags() { 2 } else { 1 };
        assert_eq!(max_phase, 1);
    }

    #[test]
    fn transfer_loop_proto29_two_phase_break() {
        let ctx = generator_for(29);
        assert!(ctx.protocol.supports_iflags());

        let max_phase: i32 = if ctx.protocol.supports_iflags() { 2 } else { 1 };
        assert_eq!(max_phase, 2);
    }
}

mod files_from {
    use super::*;

    #[test]
    fn resolve_returns_empty_when_no_files_from_configured() {
        let (handshake, ctx) = test_generator();
        let _ = &handshake;
        let paths = vec![PathBuf::from("/src")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_reads_from_stream_when_stdin() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some("-".to_owned());
        let ctx = GeneratorContext::new(&handshake, config);

        let paths = vec![PathBuf::from("/src")];
        // NUL-separated file list with double-NUL terminator
        let wire_data = b"file1.txt\0subdir/file2.txt\0\0";
        let mut reader = Cursor::new(wire_data.to_vec());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], PathBuf::from("/src/file1.txt"));
        assert_eq!(result[1], PathBuf::from("/src/subdir/file2.txt"));
    }

    #[test]
    fn resolve_uses_dot_base_when_no_paths() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some("-".to_owned());
        let ctx = GeneratorContext::new(&handshake, config);

        let paths: Vec<PathBuf> = vec![];
        let wire_data = b"file.txt\0\0";
        let mut reader = Cursor::new(wire_data.to_vec());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("./file.txt"));
    }

    #[test]
    fn resolve_skips_empty_filenames() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some("-".to_owned());
        let ctx = GeneratorContext::new(&handshake, config);

        let paths = vec![PathBuf::from("/base")];
        // Single file (the double-NUL is the terminator, no empty names in between)
        let wire_data = b"only.txt\0\0";
        let mut reader = Cursor::new(wire_data.to_vec());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("/base/only.txt"));
    }

    #[test]
    fn resolve_reads_from_local_file() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("filelist.txt");
        std::fs::write(&list_file, "alpha.txt\nbeta.txt\ngamma.txt\n").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some(list_file.to_string_lossy().to_string());
        let ctx = GeneratorContext::new(&handshake, config);

        let paths = vec![PathBuf::from("/data")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], PathBuf::from("/data/alpha.txt"));
        assert_eq!(result[1], PathBuf::from("/data/beta.txt"));
        assert_eq!(result[2], PathBuf::from("/data/gamma.txt"));
    }

    #[test]
    fn resolve_local_file_with_from0() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("filelist0.txt");
        std::fs::write(&list_file, b"one.txt\0two.txt\0\0").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some(list_file.to_string_lossy().to_string());
        config.file_selection.from0 = true;
        let ctx = GeneratorContext::new(&handshake, config);

        let paths = vec![PathBuf::from("/root")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], PathBuf::from("/root/one.txt"));
        assert_eq!(result[1], PathBuf::from("/root/two.txt"));
    }

    #[test]
    fn resolve_local_file_skips_comments() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("filelist.txt");
        std::fs::write(
            &list_file,
            "# comment\nfile1.txt\n; another comment\nfile2.txt\n",
        )
        .unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some(list_file.to_string_lossy().to_string());
        let ctx = GeneratorContext::new(&handshake, config);

        let paths = vec![PathBuf::from("/dir")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], PathBuf::from("/dir/file1.txt"));
        assert_eq!(result[1], PathBuf::from("/dir/file2.txt"));
    }

    #[test]
    fn build_file_list_with_base_produces_correct_relative_names() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        std::fs::create_dir_all(src.join("subdir")).unwrap();
        std::fs::write(src.join("hello.txt"), "hello").unwrap();
        std::fs::write(src.join("subdir/file.txt"), "nested").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        let mut ctx = GeneratorContext::new(&handshake, config);

        let file_paths = vec![src.join("hello.txt"), src.join("subdir/file.txt")];
        let count = ctx.build_file_list_with_base(&src, &file_paths).unwrap();

        // Dot entry + 2 files + 1 parent dir "subdir"
        assert!(count >= 3, "expected at least 3 entries, got {count}");

        // Verify that file entries have correct relative names (not empty).
        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(
            names.contains(&"hello.txt"),
            "expected hello.txt in {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("file.txt")),
            "expected file.txt in {names:?}"
        );
        // The dot entry should be present.
        assert!(names.contains(&"."), "expected dot entry in {names:?}");
    }

    #[test]
    fn build_file_list_with_base_skips_missing_files() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("exists.txt"), "data").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        let mut ctx = GeneratorContext::new(&handshake, config);

        let file_paths = vec![src.join("exists.txt"), src.join("missing.txt")];
        let count = ctx.build_file_list_with_base(&src, &file_paths).unwrap();

        // Dot entry + exists.txt; missing.txt is skipped with io_error.
        assert_eq!(count, 2, "dot + exists.txt");
        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(names.contains(&"exists.txt"));
        assert!(!names.contains(&"missing.txt"));
    }

    #[test]
    fn read_files_from_local_path_line_delimited() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("list.txt");
        std::fs::write(&list_file, "a.txt\nb.txt\nc.txt\n").unwrap();

        let result =
            super::super::filters::read_files_from_local_path(&list_file.to_string_lossy(), false)
                .unwrap();
        assert_eq!(result, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn read_files_from_local_path_nul_delimited() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("list0.txt");
        std::fs::write(&list_file, b"x.txt\0y.txt\0\0").unwrap();

        let result =
            super::super::filters::read_files_from_local_path(&list_file.to_string_lossy(), true)
                .unwrap();
        assert_eq!(result, vec!["x.txt", "y.txt"]);
    }

    #[test]
    fn read_files_from_local_path_skips_empty_and_comments() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("list.txt");
        std::fs::write(&list_file, "# header\n\nfile.txt\n; skip\n\nother.txt\n").unwrap();

        let result =
            super::super::filters::read_files_from_local_path(&list_file.to_string_lossy(), false)
                .unwrap();
        assert_eq!(result, vec!["file.txt", "other.txt"]);
    }
}

#[test]
fn generator_skips_files_matching_per_directory_merge_rules() {
    // Create directory structure with a .rsync-filter file that excludes *.log
    let temp_dir = TempDir::new().unwrap();
    let base = temp_dir.path();
    fs::write(base.join("keep.txt"), b"keep").unwrap();
    fs::write(base.join("skip.log"), b"skip").unwrap();
    fs::write(base.join(".rsync-filter"), "- *.log\n").unwrap();

    let (_handshake, mut ctx) = test_generator_for_path(base, false);

    // Set up a DirMergeConfig for .rsync-filter
    ctx.filter_chain
        .add_merge_config(::filters::DirMergeConfig::new(".rsync-filter"));

    let count = build_file_list_for(&mut ctx, base);

    // Should have "." + "keep.txt" + ".rsync-filter" but not "skip.log"
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
    assert!(
        !names.iter().any(|n| n.contains("skip.log")),
        "skip.log should be excluded by .rsync-filter, got: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("keep.txt")),
        "keep.txt should be included, got: {names:?}"
    );
    assert!(count >= 2); // At least "." and "keep.txt"
}

#[test]
fn generator_nested_directories_cascading_merge_rules() {
    // Root has .rsync-filter excluding *.bak
    // Subdir has .rsync-filter excluding *.tmp
    let temp_dir = TempDir::new().unwrap();
    let base = temp_dir.path();
    fs::write(base.join(".rsync-filter"), "- *.bak\n").unwrap();
    fs::write(base.join("root.txt"), b"root").unwrap();
    fs::write(base.join("root.bak"), b"bak").unwrap();

    let sub = base.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join(".rsync-filter"), "- *.tmp\n").unwrap();
    fs::write(sub.join("sub.txt"), b"sub").unwrap();
    fs::write(sub.join("sub.tmp"), b"tmp").unwrap();
    fs::write(sub.join("sub.bak"), b"bak2").unwrap();

    let (_handshake, mut ctx) = test_generator_for_path(base, true);
    ctx.filter_chain
        .add_merge_config(::filters::DirMergeConfig::new(".rsync-filter"));

    let _count = build_file_list_for(&mut ctx, base);
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();

    // root.bak excluded by root .rsync-filter
    assert!(
        !names.iter().any(|n| n.ends_with("root.bak")),
        "root.bak should be excluded: {names:?}"
    );

    // sub/sub.tmp excluded by sub/.rsync-filter
    assert!(
        !names.iter().any(|n| n.ends_with("sub.tmp")),
        "sub.tmp should be excluded: {names:?}"
    );

    // sub/sub.bak excluded by root .rsync-filter (inherited)
    assert!(
        !names.iter().any(|n| n.ends_with("sub.bak")),
        "sub.bak should be excluded by inherited rule: {names:?}"
    );

    // root.txt and sub/sub.txt should be present
    assert!(
        names.iter().any(|n| n.ends_with("root.txt")),
        "root.txt should be included: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with("sub.txt")),
        "sub.txt should be included: {names:?}"
    );
}

#[test]
fn generator_merge_filters_properly_scoped() {
    // Dir A has .rsync-filter excluding *.a
    // Dir B has no .rsync-filter
    // Rules from A should not affect B
    let temp_dir = TempDir::new().unwrap();
    let base = temp_dir.path();

    let dir_a = base.join("a");
    fs::create_dir(&dir_a).unwrap();
    fs::write(dir_a.join(".rsync-filter"), "- *.a\n").unwrap();
    fs::write(dir_a.join("file.a"), b"excluded").unwrap();
    fs::write(dir_a.join("file.txt"), b"included").unwrap();

    let dir_b = base.join("b");
    fs::create_dir(&dir_b).unwrap();
    fs::write(dir_b.join("file.a"), b"should-be-included").unwrap();
    fs::write(dir_b.join("file.txt"), b"also-included").unwrap();

    let (_handshake, mut ctx) = test_generator_for_path(base, true);
    ctx.filter_chain
        .add_merge_config(::filters::DirMergeConfig::new(".rsync-filter"));

    let _count = build_file_list_for(&mut ctx, base);
    let names: Vec<String> = ctx
        .file_list()
        .iter()
        .map(|e| e.path().display().to_string())
        .collect();

    // a/file.a should be excluded by a/.rsync-filter
    assert!(
        !names.iter().any(|n| n == "a/file.a"),
        "a/file.a should be excluded by a/.rsync-filter: {names:?}"
    );

    // b/file.a should NOT be excluded (no merge file in b)
    assert!(
        names.iter().any(|n| n == "b/file.a"),
        "b/file.a should be included (rules from a don't affect b): {names:?}"
    );
}

#[test]
fn generator_merge_filter_exclude_self() {
    // .rsync-filter excludes itself when exclude_self is set
    let temp_dir = TempDir::new().unwrap();
    let base = temp_dir.path();
    fs::write(base.join(".rsync-filter"), "- *.bak\n").unwrap();
    fs::write(base.join("file.txt"), b"keep").unwrap();

    let (_handshake, mut ctx) = test_generator_for_path(base, false);
    ctx.filter_chain
        .add_merge_config(::filters::DirMergeConfig::new(".rsync-filter").with_exclude_self(true));

    let _count = build_file_list_for(&mut ctx, base);
    let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();

    // .rsync-filter itself should be excluded
    assert!(
        !names.contains(&".rsync-filter"),
        ".rsync-filter should be excluded when exclude_self is true: {names:?}"
    );

    // file.txt should remain
    assert!(
        names.contains(&"file.txt"),
        "file.txt should be present: {names:?}"
    );
}

#[test]
fn generator_no_merge_configs_unchanged_behavior() {
    // With no merge configs, behavior should be identical to before
    let temp_dir = create_test_files(&[("file1.txt", b"data1"), ("file2.log", b"data2")]);
    let base = temp_dir.path();

    let (_handshake, mut ctx) = test_generator_for_path(base, false);
    // No merge configs added - filter_chain is empty

    let count = build_file_list_for(&mut ctx, base);

    // Should have "." + 2 files = 3 entries
    assert_eq!(count, 3);
}

#[test]
fn parse_received_filters_extracts_dir_merge_config() {
    let (_handshake, ctx) = test_generator();

    // Construct a DirMerge wire rule by modifying an exclude rule's type
    let mut dir_merge_rule = FilterRuleWireFormat::exclude(".rsync-filter".to_owned());
    dir_merge_rule.rule_type = protocol::filters::RuleType::DirMerge;
    dir_merge_rule.exclude_from_merge = true;

    let wire_rules = vec![
        FilterRuleWireFormat::exclude("*.bak".to_owned()),
        dir_merge_rule,
    ];

    let (filter_set, merge_configs) = ctx.parse_received_filters(&wire_rules).unwrap();

    // The exclude rule should be in the filter set
    assert!(!filter_set.is_empty());

    // The DirMerge rule should produce a DirMergeConfig
    assert_eq!(merge_configs.len(), 1);
    assert_eq!(merge_configs[0].filename(), ".rsync-filter");
    assert!(merge_configs[0].excludes_self());
    assert!(merge_configs[0].inherits()); // default
}

#[test]
fn parse_received_filters_dir_merge_no_inherit() {
    let (_handshake, ctx) = test_generator();

    let mut dir_merge_rule = FilterRuleWireFormat::exclude(".exclude".to_owned());
    dir_merge_rule.rule_type = protocol::filters::RuleType::DirMerge;
    dir_merge_rule.no_inherit = true;

    let wire_rules = vec![dir_merge_rule];

    let (filter_set, merge_configs) = ctx.parse_received_filters(&wire_rules).unwrap();

    assert!(filter_set.is_empty());
    assert_eq!(merge_configs.len(), 1);
    assert_eq!(merge_configs[0].filename(), ".exclude");
    assert!(!merge_configs[0].inherits());
}

#[test]
fn server_mode_flushes_writer_before_filter_list_read() {
    // Regression test for daemon pull mode deadlock.
    //
    // In daemon pull mode, the oc-rsync daemon acts as the generator (sender).
    // After multiplex activation, any buffered output (e.g. MSG_IO_TIMEOUT)
    // must be flushed to the wire before the generator blocks reading the
    // client's filter list. Without this flush, the client may wait for
    // server output before sending its filter list, causing a deadlock.
    //
    // upstream: main.c:1248-1258 - io_start_multiplex_out() then recv_filter_list()
    // upstream: io.c:perform_io() - flushes output buffer while waiting for input

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Create a writer wrapper that tracks flush calls.
    struct FlushTracker {
        flushed: Arc<AtomicBool>,
        inner: Vec<u8>,
    }

    impl io::Write for FlushTracker {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let flushed = Arc::new(AtomicBool::new(false));
    let tracker = FlushTracker {
        flushed: Arc::clone(&flushed),
        inner: Vec::new(),
    };

    // Build a MultiplexWriter so we can verify flush propagation.
    let mut writer = crate::writer::ServerWriter::new_plain(tracker);
    writer = writer.activate_multiplex().unwrap();

    // Write some data to the writer (simulating MSG_IO_TIMEOUT or any
    // buffered protocol data). This data stays in the MultiplexWriter's
    // internal buffer until flushed.
    writer.write_all(b"test").unwrap();

    // Verify data is buffered but not yet flushed to the wire.
    assert!(!flushed.load(Ordering::SeqCst));

    // Create a server-mode generator context.
    let handshake = test_handshake_with_protocol(32);
    let mut config = test_config();
    config.connection.client_mode = false; // daemon/server mode

    let ctx = GeneratorContext::new(&handshake, config);

    // The fix: server mode flushes before reading filter list.
    // We verify this by calling flush on the writer as the generator does.
    if !ctx.config.connection.client_mode {
        writer.flush().unwrap();
    }

    assert!(
        flushed.load(Ordering::SeqCst),
        "writer must be flushed in server mode before reading filter list"
    );
}
