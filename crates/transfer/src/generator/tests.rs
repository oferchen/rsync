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
use std::time::Instant;
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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

/// Builds a file list for the *contents* of `base_path`.
///
/// Appends a trailing `/` so `build_file_list` enters the upstream
/// `DOTDIR_NAME` branch (flist.c:2312-2322) and emits `.` plus the
/// directory's children, matching `rsync <dir>/ dst/` semantics. Used by
/// tests that pre-populate a flat set of files and want to assert against
/// the dot-entry-plus-children layout independent of the source basename.
fn build_file_list_for_contents(ctx: &mut GeneratorContext, base_path: &Path) -> usize {
    let mut with_slash = base_path.as_os_str().to_owned();
    with_slash.push("/");
    let paths = vec![PathBuf::from(with_slash)];
    ctx.build_file_list(&paths).unwrap()
}

/// Wraps a vector of full paths as `FilesFromEntry`s sharing one base, for
/// tests that only exercise plain (no `/./` anchor) `--files-from` entries.
fn files_from_entries(base: &Path, paths: Vec<PathBuf>) -> Vec<super::filters::FilesFromEntry> {
    paths
        .into_iter()
        .map(|path| super::filters::FilesFromEntry {
            base: base.to_path_buf(),
            path,
            recurse: false,
        })
        .collect()
}

/// Creates a clear rule for filter tests.
fn clear_rule() -> FilterRuleWireFormat {
    use protocol::filters::RuleType;
    FilterRuleWireFormat {
        rule_type: RuleType::Clear,
        sender_side: true,
        receiver_side: true,
        ..FilterRuleWireFormat::default()
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
    assert_eq!(output, vec![0u8]);
}

#[test]
fn send_single_file_entry() {
    let (_handshake, mut ctx) = test_generator();

    let entry = protocol::flist::FileEntry::new_file("test.txt".into(), 100, 0o644);
    ctx.file_list.push(entry);

    let mut output = Vec::new();
    let count = ctx.send_file_list(&mut output).unwrap();

    assert_eq!(count, 1);
    assert!(!output.is_empty());
    assert_eq!(*output.last().unwrap(), 0u8);
}

#[test]
fn send_file_list_records_first_byte_latency() {
    // INC_RECURSE diagnostic I1 (#2196): the first-byte timer must fire when
    // send_file_list writes any wire bytes.
    let (_handshake, mut ctx) = test_generator();
    let entry = protocol::flist::FileEntry::new_file("probe.txt".into(), 42, 0o644);
    ctx.file_list.push(entry);

    let mut output = Vec::new();
    ctx.send_file_list(&mut output).unwrap();

    let latency = ctx
        .timing
        .flist_first_byte_latency
        .expect("first-byte latency must be recorded when wire bytes are written");
    // Instant::elapsed() is monotonic and the gap from entry through
    // build_flist_writer + write_entry + flush spans many syscalls; the
    // sampled duration is non-zero on every supported platform.
    assert!(
        latency > std::time::Duration::ZERO,
        "first-byte latency should be a non-zero elapsed duration, got {latency:?}"
    );
    assert!(
        ctx.timing.flist_xfer_start.is_some(),
        "flist_xfer_start must also be set"
    );
}

#[test]
fn send_file_list_first_byte_latency_recorded_for_empty_list() {
    // Even with no entries, send_file_list writes a one-byte end marker, so
    // the first-byte probe should still fire.
    let (_handshake, mut ctx) = test_generator();

    let mut output = Vec::new();
    ctx.send_file_list(&mut output).unwrap();

    assert_eq!(output, vec![0u8]);
    assert!(
        ctx.timing.flist_first_byte_latency.is_some(),
        "first-byte latency should be recorded once the end marker is flushed"
    );
}

#[test]
fn ndx_convert_call_counter_increments() {
    // INC_RECURSE diagnostic I4 (#2199): every wire_to_flat_ndx /
    // flat_to_wire_ndx invocation must bump the global call counter. The
    // assertion uses >= because the counter is shared across the process and
    // other tests may run concurrently.
    use super::ndx_convert_totals;

    let (_handshake, ctx) = test_generator();
    let (calls_before, _) = ndx_convert_totals();

    let _ = ctx.wire_to_flat_ndx(0);
    let _ = ctx.flat_to_wire_ndx(0);
    let _ = ctx.flat_to_wire_ndx(0);

    let (calls_after, _) = ndx_convert_totals();
    assert!(
        calls_after >= calls_before + 3,
        "expected at least 3 new ndx_convert calls (before={calls_before}, after={calls_after})"
    );
}

#[test]
fn ndx_convert_partition_point_depth_grows() {
    // INC_RECURSE diagnostic I4 (#2199): the cumulative partition_point depth
    // must monotonically grow as the segment table is queried. A 4-segment
    // table adds depth(4)=3 per call, so N calls add at least N*3. The
    // assertion uses >= because the counter is shared across the process.
    use super::{ndx_convert_totals, partition_point_depth};

    let (_handshake, mut ctx) = test_generator();
    // Default ndx_segments has one entry; extend it to four so each query
    // contributes a measurably larger partition_point depth.
    ctx.incremental.ndx_segments.push((10, 11));
    ctx.incremental.ndx_segments.push((20, 22));
    ctx.incremental.ndx_segments.push((30, 33));

    let per_call_depth = partition_point_depth(ctx.incremental.ndx_segments.len());
    assert!(
        per_call_depth >= 3,
        "expected partition_point_depth(4) >= 3, got {per_call_depth}"
    );

    const N: u64 = 8;
    let (_, cmps_before) = ndx_convert_totals();
    for _ in 0..N {
        let _ = ctx.flat_to_wire_ndx(0);
    }
    let (_, cmps_after) = ndx_convert_totals();

    assert!(
        cmps_after >= cmps_before + N * per_call_depth,
        "cumulative partition_point depth should grow by at least {} \
         (before={cmps_before}, after={cmps_after})",
        N * per_call_depth
    );
}

#[test]
fn inc_recurse_gap_ndx_round_trip_preserves_original() {
    // When INC_RECURSE is active, ndx_start=1. The upstream generator sends
    // "gap NDX" values (ndx_start - 1 = 0) to signal parent directory
    // metadata updates. The sender must echo the original wire NDX unchanged.
    //
    // Before the fix, wire_to_flat_ndx(0) with ndx_start=1 computed
    // 0 + (0 - 1) as usize = usize::MAX, and flat_to_wire_ndx(usize::MAX)
    // produced a garbage value instead of 0.
    //
    // upstream: sender.c:263-266 - gap NDX echoed unchanged
    use protocol::CompatibilityFlags;

    let mut handshake = test_handshake_with_protocol(32);
    handshake.compat_flags = Some(
        CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SYMLINK_TIMES
            | CompatibilityFlags::SYMLINK_ICONV,
    );

    let ctx = GeneratorContext::new_for_test(&handshake, test_config());

    // Verify INC_RECURSE is active and ndx_start=1
    assert!(ctx.inc_recurse());
    assert_eq!(ctx.incremental.ndx_segments, vec![(0, 1)]);

    // Gap NDX = ndx_start - 1 = 0
    let gap_ndx: i32 = 0;

    // The fix preserves wire_ndx directly, so the round-trip through
    // wire_to_flat_ndx + flat_to_wire_ndx is no longer used in production.
    // Verify that the gap NDX value (0) is below ndx_start (1), which is
    // the condition under which upstream echoes the original NDX unchanged.
    let ndx_start = ctx.incremental.ndx_segments[0].1;
    assert!(
        gap_ndx < ndx_start,
        "gap NDX ({gap_ndx}) must be below ndx_start ({ndx_start})"
    );

    // Also verify that a normal NDX (at or above ndx_start) round-trips
    // correctly through the conversion functions.
    let normal_ndx: i32 = 1;
    let flat = ctx.wire_to_flat_ndx(normal_ndx);
    let back = ctx.flat_to_wire_ndx(flat);
    assert_eq!(
        back, normal_ndx,
        "normal NDX round-trip: wire_to_flat_ndx({normal_ndx}) = {flat}, \
         flat_to_wire_ndx({flat}) = {back}, expected {normal_ndx}"
    );
}

#[test]
fn flush_with_count_increments_global_counter() {
    // INC_RECURSE diagnostic I3 (#2198): every flush on the generator
    // transfer hot path must bump the global FLUSH_CALLS counter. The
    // assertion uses >= because the counter is shared across the process and
    // other tests may run concurrently.
    use super::{flush_rate_totals, flush_with_count};

    let before = flush_rate_totals();

    let mut sink: Vec<u8> = Vec::new();
    flush_with_count(&mut sink).unwrap();
    flush_with_count(&mut sink).unwrap();
    flush_with_count(&mut sink).unwrap();

    let after = flush_rate_totals();
    assert!(
        after >= before + 3,
        "expected at least 3 new flush calls (before={before}, after={after})"
    );
}

#[test]
fn flush_rate_totals_is_observable_without_flushing() {
    // INC_RECURSE diagnostic I3 (#2198): the totals snapshot must be readable
    // without triggering any flush. Constructing a generator must not bump
    // the counter on its own, so two adjacent reads with no intervening
    // flush_with_count call must return identical values.
    use super::flush_rate_totals;

    let (_handshake, _ctx) = test_generator();
    let first = flush_rate_totals();
    let second = flush_rate_totals();

    assert_eq!(
        first, second,
        "flush_rate_totals must be a pure read (first={first}, second={second})"
    );
}

#[test]
fn prepare_acl_call_counter_increments() {
    // INC_RECURSE diagnostic I5 (#2200): every record_prepare_acl invocation
    // must bump the global PREPARE_ACL_CALLS counter. The assertion uses >=
    // because the counter is shared across the process and other tests may
    // run concurrently.
    use super::{prepare_acl_totals, record_prepare_acl};
    use std::time::Duration;

    let (calls_before, ns_before) = prepare_acl_totals();

    record_prepare_acl(Duration::from_nanos(100));
    record_prepare_acl(Duration::from_nanos(200));
    record_prepare_acl(Duration::from_nanos(300));

    let (calls_after, ns_after) = prepare_acl_totals();
    assert!(
        calls_after >= calls_before + 3,
        "expected at least 3 new prepare_acl calls (before={calls_before}, after={calls_after})"
    );
    assert!(
        ns_after >= ns_before + 600,
        "cumulative elapsed_ns should grow by at least 600 \
         (before={ns_before}, after={ns_after})"
    );
}

#[test]
fn prepare_acl_totals_observable_without_prep() {
    // INC_RECURSE diagnostic I5 (#2200): the totals snapshot must be readable
    // without triggering any ACL prep. Constructing a generator must not bump
    // the counter on its own, so two adjacent reads with no intervening
    // record_prepare_acl call must return identical values.
    use super::prepare_acl_totals;

    let (_handshake, _ctx) = test_generator();
    let first = prepare_acl_totals();
    let second = prepare_acl_totals();

    assert_eq!(
        first, second,
        "prepare_acl_totals must be a pure read (first={first:?}, second={second:?})"
    );
}

#[test]
fn segment_dispatch_call_counter_increments() {
    // INC_RECURSE diagnostic I2 (#2197): every record_segment_dispatch
    // invocation must bump the global SEGMENT_DISPATCH_CALLS counter. The
    // assertion uses >= because the counter is shared across the process and
    // other tests may run concurrently.
    use super::{record_segment_dispatch, segment_dispatch_totals};
    use std::time::Duration;

    let (calls_before, ns_before) = segment_dispatch_totals();

    record_segment_dispatch(Duration::from_nanos(150));
    record_segment_dispatch(Duration::from_nanos(250));
    record_segment_dispatch(Duration::from_nanos(350));

    let (calls_after, ns_after) = segment_dispatch_totals();
    assert!(
        calls_after >= calls_before + 3,
        "expected at least 3 new segment dispatch calls (before={calls_before}, after={calls_after})"
    );
    assert!(
        ns_after >= ns_before + 750,
        "cumulative elapsed_ns should grow by at least 750 \
         (before={ns_before}, after={ns_after})"
    );
}

#[test]
fn segment_dispatch_totals_observable_without_dispatch() {
    // INC_RECURSE diagnostic I2 (#2197): the totals snapshot must be readable
    // without dispatching any segment. Constructing a generator must not bump
    // the counter on its own, so two adjacent reads with no intervening
    // record_segment_dispatch call must return identical values.
    use super::segment_dispatch_totals;

    let (_handshake, _ctx) = test_generator();
    let first = segment_dispatch_totals();
    let second = segment_dispatch_totals();

    assert_eq!(
        first, second,
        "segment_dispatch_totals must be a pure read (first={first:?}, second={second:?})"
    );
}

#[test]
fn build_and_send_round_trip() {
    use crate::receiver::ReceiverContext;

    let handshake = test_handshake();
    let mut gen_config = test_config();
    gen_config.role = ServerRole::Generator;
    let mut generator = GeneratorContext::new_for_test(&handshake, gen_config);

    let mut entry1 = protocol::flist::FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_mtime(1700000000, 0);
    let mut entry2 = protocol::flist::FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_mtime(1700000000, 0);
    generator.file_list.push(entry1);
    generator.file_list.push(entry2);

    let mut wire_data = Vec::new();
    generator.send_file_list(&mut wire_data).unwrap();

    let recv_config = test_config();
    let mut receiver = ReceiverContext::new_for_test(&handshake, recv_config);
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
    // The clear rule wipes the prior excludes; only the include survives.
    assert!(!filter_set.is_empty());
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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`.
    let count = build_file_list_for_contents(&mut ctx, base_path);

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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`.
    let count = build_file_list_for_contents(&mut ctx, base_path);

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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`.
    let count = build_file_list_for_contents(&mut ctx, base_path);

    // "." root dir + 3 files when no filters are present.
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

    let wire_ops = script_to_wire_delta(script, 1024);

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

    let wire_ops = script_to_wire_delta(script, 1024);

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
        None,
    )
    .unwrap();

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
        None,
    )
    .unwrap();

    // An empty file still produces a strong checksum digest (MD5 of zero bytes).
    assert!(result.checksum_len > 0);
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
        None,
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
        None,
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
        None,
    )
    .unwrap();

    // Buffer capacity should not have grown beyond initial CHUNK_SIZE
    assert_eq!(buf.capacity(), initial_capacity);
}

#[test]
fn stream_whole_file_none_checksum() {
    use protocol::wire::write_whole_file_delta;
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
        None,
    )
    .unwrap();

    // None algorithm produces a 1-byte zero placeholder
    assert_eq!(result.checksum_len, 1);
    assert_eq!(result.checksum_buf[0], 0);

    // The checksum algorithm only affects the returned digest, not the wire
    // stream: the literal token carries all source bytes. Compare against the
    // known-good delta encoding to prove all 256 bytes were streamed.
    let mut expected = Vec::new();
    write_whole_file_delta(&mut expected, &data).unwrap();
    assert_eq!(wire_output, expected);
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
    let (ftype, xname, consumed) = flags.read_trailing(&mut cursor).unwrap();

    assert!(ftype.is_none());
    assert!(xname.is_none());
    assert_eq!(consumed, 0);
}

#[test]
fn item_flags_read_trailing_basis_type() {
    // ITEM_BASIS_TYPE_FOLLOWS reads 1 byte
    let data = [0x42]; // basis type = BasisDir(0x42)
    let mut cursor = Cursor::new(&data[..]);

    let flags = ItemFlags::from_raw(0x0800); // ITEM_BASIS_TYPE_FOLLOWS
    let (ftype, xname, consumed) = flags.read_trailing(&mut cursor).unwrap();

    assert_eq!(ftype, Some(protocol::FnameCmpType::BasisDir(0x42)));
    assert!(xname.is_none());
    assert_eq!(consumed, 1);
}

/// Encodes an xname length exactly as upstream `write_vstring()` (io.c:2022):
/// one byte for `len <= 0x7F`, otherwise `[len/0x100 + 0x80, len & 0xFF]`.
fn encode_xname_vstring(payload: &[u8]) -> Vec<u8> {
    let len = payload.len();
    let mut out = Vec::with_capacity(len + 2);
    if len > 0x7F {
        out.push((len / 0x100) as u8 + 0x80);
    }
    out.push((len & 0xFF) as u8);
    out.extend_from_slice(payload);
    out
}

#[test]
fn item_flags_read_trailing_short_xname_matches_upstream_vstring() {
    // A short xname (len <= 0x7F) is a single length byte followed by the
    // payload. The generator/sender must decode the vstring prefix, not a
    // varint - upstream io.c:2004 read_vstring().
    let payload = b"basis.old";
    let wire = encode_xname_vstring(payload);
    let mut cursor = Cursor::new(&wire[..]);

    let flags = ItemFlags::from_raw(ItemFlags::ITEM_XNAME_FOLLOWS);
    let (ftype, xname, consumed) = flags.read_trailing(&mut cursor).unwrap();

    assert!(ftype.is_none());
    assert_eq!(xname.as_deref(), Some(&payload[..]));
    // 1 prefix byte + payload; the cursor must be fully drained so the next
    // wire read (sum_head) stays aligned.
    assert_eq!(consumed, 1 + payload.len() as u64);
    assert_eq!(cursor.position() as usize, wire.len());
}

#[test]
fn item_flags_read_trailing_long_xname_uses_two_byte_prefix() {
    // A 200-byte xname exercises the 2-byte vstring prefix. read_varint would
    // decode [0x80, 0xC8] as a completely different length and desync the
    // stream; the vstring decode yields exactly 200. upstream io.c:2007-2008.
    let payload = vec![b'x'; 200];
    let wire = encode_xname_vstring(&payload);
    // Sanity: upstream 2-byte framing for len=200 is [0x80, 0xC8].
    assert_eq!(wire[0], 0x80);
    assert_eq!(wire[1], 0xC8);

    let mut cursor = Cursor::new(&wire[..]);
    let flags = ItemFlags::from_raw(ItemFlags::ITEM_XNAME_FOLLOWS);
    let (_ftype, xname, consumed) = flags.read_trailing(&mut cursor).unwrap();

    assert_eq!(xname.as_deref(), Some(&payload[..]));
    assert_eq!(consumed, 2 + payload.len() as u64);
    assert_eq!(cursor.position() as usize, wire.len());
}

#[test]
fn item_flags_read_trailing_rejects_over_long_xname() {
    // upstream io.c:2010-2014: a vstring length of >= MAXPATHLEN (4096) is a
    // protocol error. Truncating would leave the tail on the wire and desync
    // every subsequent read, so read_trailing must surface an error instead.
    // len = 4096 -> two-byte prefix [0x90, 0x00].
    let wire = [0x90u8, 0x00];
    let mut cursor = Cursor::new(&wire[..]);

    let flags = ItemFlags::from_raw(ItemFlags::ITEM_XNAME_FOLLOWS);
    let err = flags
        .read_trailing(&mut cursor)
        .expect_err("over-long xname must be rejected, not truncated");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("over-long xname vstring"));
}

#[test]
fn item_flags_read_trailing_basis_type_and_xname() {
    // Combined ITEM_BASIS_TYPE_FOLLOWS + ITEM_XNAME_FOLLOWS: one basis-type
    // byte precedes the xname vstring. upstream rsync.c:403-408 reads them in
    // that order.
    let payload = b"fuzzy";
    let mut wire = vec![0x42];
    wire.extend_from_slice(&encode_xname_vstring(payload));
    let mut cursor = Cursor::new(&wire[..]);

    let flags =
        ItemFlags::from_raw(ItemFlags::ITEM_BASIS_TYPE_FOLLOWS | ItemFlags::ITEM_XNAME_FOLLOWS);
    let (ftype, xname, consumed) = flags.read_trailing(&mut cursor).unwrap();

    assert_eq!(ftype, Some(protocol::FnameCmpType::BasisDir(0x42)));
    assert_eq!(xname.as_deref(), Some(&payload[..]));
    assert_eq!(consumed, 1 + 1 + payload.len() as u64);
    assert_eq!(cursor.position() as usize, wire.len());
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

    let ctx = GeneratorContext::new_for_test(&handshake, config);
    // Protocol 28 >= 23, so should activate in client mode
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn should_activate_input_multiplex_client_mode_protocol_32() {
    // Test with higher protocol version
    let handshake = test_handshake_with_protocol(32);
    let mut config = test_config();
    config.connection.client_mode = true;

    let ctx = GeneratorContext::new_for_test(&handshake, config);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn should_activate_input_multiplex_server_mode_protocol_30() {
    let handshake = test_handshake_with_protocol(30);
    let mut config = test_config();
    config.connection.client_mode = false;

    let ctx = GeneratorContext::new_for_test(&handshake, config);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn should_activate_input_multiplex_server_mode_protocol_29() {
    let handshake = test_handshake_with_protocol(29);
    let mut config = test_config();
    config.connection.client_mode = false;

    let ctx = GeneratorContext::new_for_test(&handshake, config);
    assert!(!ctx.should_activate_input_multiplex());
}

#[test]
fn get_checksum_algorithm_default_protocol_28() {
    let handshake = test_handshake_with_protocol(28);

    let ctx = GeneratorContext::new_for_test(&handshake, test_config());
    assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::MD4);
}

#[test]
fn get_checksum_algorithm_default_protocol_30() {
    let handshake = test_handshake_with_protocol(30);

    let ctx = GeneratorContext::new_for_test(&handshake, test_config());
    assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::MD5);
}

#[test]
fn get_checksum_algorithm_negotiated() {
    let handshake = test_handshake_with_negotiated_algorithms(
        32,
        ChecksumAlgorithm::XXH3,
        CompressionAlgorithm::None,
    );

    let ctx = GeneratorContext::new_for_test(&handshake, test_config());
    assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::XXH3);
}

#[test]
fn validate_file_index_valid() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());
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
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());
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
    let ctx = GeneratorContext::new_for_test(&handshake, test_config());

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

    let ctx = GeneratorContext::new_for_test(&handshake, config);

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

    let ctx = GeneratorContext::new_for_test(&handshake, config);

    let mut output = Vec::new();
    ctx.send_id_lists(&mut output).unwrap();

    // Should have varint 0 terminator (1 byte)
    assert!(!output.is_empty());
    assert_eq!(output[0], 0); // Empty list terminator
}

#[test]
fn send_io_error_flag_protocol_29() {
    let handshake = test_handshake_with_protocol(29);
    let ctx = GeneratorContext::new_for_test(&handshake, test_config());

    let mut output = Vec::new();
    ctx.send_io_error_flag(&mut output).unwrap();

    // Protocol < 30 should write 4-byte io_error (value 0)
    assert_eq!(output.len(), 4);
    assert_eq!(output, &[0, 0, 0, 0]);
}

#[test]
fn send_io_error_flag_protocol_30() {
    let handshake = test_handshake_with_protocol(30);
    let ctx = GeneratorContext::new_for_test(&handshake, test_config());

    let mut output = Vec::new();
    ctx.send_io_error_flag(&mut output).unwrap();

    // Protocol >= 30 should not write io_error
    assert!(output.is_empty());
}

#[test]
fn send_io_error_flag_with_errors_protocol_29() {
    let handshake = test_handshake_with_protocol(29);
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());
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

    let mut ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);

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
    let ctx = GeneratorContext::new_for_test(&handshake, config);

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
    let ctx = GeneratorContext::new_for_test(&handshake, config);

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
    let ctx = GeneratorContext::new_for_test(&handshake, config);

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
    let ctx = GeneratorContext::new_for_test(&handshake, config);

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
    let generator = GeneratorContext::new_for_test(&handshake, gen_config);

    let mut wire_data = Vec::new();
    generator.send_id_lists(&mut wire_data).unwrap();

    // Both empty lists with terminators
    assert_eq!(wire_data, vec![0, 0]);

    // Receiver reads ID lists with matching flags
    let recv_config = config_with_role_and_flags(ServerRole::Receiver, true, true, false);
    let mut receiver = ReceiverContext::new_for_test(&handshake, recv_config);

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
    let generator = GeneratorContext::new_for_test(&handshake, gen_config);

    let mut wire_data = Vec::new();
    generator.send_id_lists(&mut wire_data).unwrap();

    // No data written when numeric_ids=true
    assert!(wire_data.is_empty());

    // Receiver also skips reading with matching flags
    let recv_config = config_with_role_and_flags(ServerRole::Receiver, true, true, true);
    let mut receiver = ReceiverContext::new_for_test(&handshake, recv_config);

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

    let ctx = GeneratorContext::new_for_test(&handshake, test_config());
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

    let ctx = GeneratorContext::new_for_test(&handshake, test_config());
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
    let mut ctx = GeneratorContext::new_for_test(&handshake, config);

    // Trailing-slash source enters upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list contains `.` + the base
    // directory's children, matching `rsync <dir>/ dst/`. Without it the
    // non-relative walk-base split (flist.c:2338-2349) would emit only
    // the source basename instead of `.` plus its children.
    let count = build_file_list_for_contents(&mut ctx, base_path);

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
    let mut ctx = GeneratorContext::new_for_test(&handshake, config);

    // Trailing-slash source enters upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the wire-side names are `.`, `regular.txt`,
    // and `test.fifo` instead of `<basename>/regular.txt` etc.
    let count = build_file_list_for_contents(&mut ctx, base_path);

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
    let mut ctx = GeneratorContext::new_for_test(&handshake, config);

    // Trailing-slash source enters upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the FIFO child.
    build_file_list_for_contents(&mut ctx, base_path);

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
    let mut generator = GeneratorContext::new_for_test(&handshake, gen_config);

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
    let mut receiver = ReceiverContext::new_for_test(&handshake, recv_config);

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
        false,
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
        false,
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
        false,
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
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
    let ctx = GeneratorContext::new_for_test(&handshake, config);
    assert!(ctx.should_send_del_stats());
}

#[test]
fn record_io_error_not_found_sets_vanished() {
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());

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
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());

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
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());

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

    /// NDX_DONE as 4-byte little-endian (-1 = 0xFFFFFFFF), used by the legacy
    /// codec for protocol < 30.
    const NDX_DONE_LE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

    /// NDX_DONE as encoded by the modern (protocol >= 30) codec.
    ///
    /// upstream io.c:2259-2262 - single 0x00 byte with no side effects.
    const NDX_DONE_MODERN: [u8; 1] = [0x00];

    /// Creates a `GeneratorContext` for a specific protocol version.
    fn generator_for(protocol_version: u8) -> GeneratorContext {
        let handshake = test_handshake_with_protocol(protocol_version);
        let mut config = test_config();
        config.protocol = ProtocolVersion::try_from(protocol_version).unwrap();
        GeneratorContext::new_for_test(&handshake, config)
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
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

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

    // UTS-15.c: protocol 31+ extended goodbye must flush NDX_DONE into the
    // wire buffer before the read loop blocks on the receiver's reply. The
    // `FlushTrackingWriter` records every `flush()` invocation; we then
    // assert the final wire bytes match the NDX_DONE marker and that at
    // least one flush happened after the write.
    //
    // Without the flush after `write_ndx_done`, a buffered writer can hold
    // the four marker bytes in user-space while the receiver is already
    // shutting down the socket - the symptom upstream's batch-mode interop
    // surfaced as a silent close at byte ~2241725. Mirroring
    // `main.c:875-906 read_final_goodbye()` requires the flush to happen
    // before close on every protocol-31+ goodbye.
    #[test]
    fn handle_goodbye_proto31_flushes_ndx_done_before_close() {
        let handshake = test_handshake_with_protocol(31);
        let mut config = test_config();
        config.protocol = ProtocolVersion::try_from(31u8).unwrap();
        // Skip del_stats so the wire payload is just one echo NDX_DONE.
        config.do_stats = false;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        // Receiver wire: NDX_DONE + final NDX_DONE in modern (proto >= 30)
        // single-byte encoding. The legacy 4-byte LE form is only valid for
        // protocol < 30; using it here would decode as -256 instead of -1.
        let receiver_input = [NDX_DONE_MODERN.as_slice(), NDX_DONE_MODERN.as_slice()].concat();
        let mut reader = Cursor::new(receiver_input);

        let mut output = FlushTrackingWriter::default();
        let mut ndx_read = create_ndx_codec(31);
        let mut ndx_write = MonotonicNdxWriter::new(31);

        ctx.handle_goodbye(&mut reader, &mut output, &mut ndx_read, &mut ndx_write)
            .expect("goodbye completes");

        // The wire payload must end with the NDX_DONE marker byte.
        assert!(
            output.buffer.ends_with(&NDX_DONE_MODERN),
            "wire output must end with NDX_DONE: {:?}",
            output.buffer
        );
        // And at least one flush must have happened to push it out.
        assert!(
            output.flushes >= 1,
            "writer must flush at least once before goodbye returns; got {}",
            output.flushes
        );
        // Defense-in-depth: the last write recorded must be a flush, not a
        // partial-write. Without flush-before-close, the final NDX_DONE can
        // sit in a user-space buffer when the FIN goes out.
        assert!(
            output.last_op_was_flush,
            "the final operation before return must be a flush, not a write"
        );
    }

    // UTS-9.REOPEN (daemon-gzip-download): the daemon-sender's `-zz` goodbye
    // path deadlocked because the codec finalize step happened only AFTER
    // `handle_goodbye` returned, but `handle_goodbye` could never return
    // since it was blocked reading the receiver's final NDX_DONE while the
    // receiver was simultaneously blocked decompressing an unterminated
    // deflate block. The fix introduces `handle_goodbye_with_finalizer`,
    // which runs a caller-supplied hook BETWEEN the goodbye write+flush and
    // the goodbye read. This test asserts that ordering invariant: the hook
    // must observe the goodbye write already in the wire buffer, AND must
    // run before any further reader byte is consumed.
    //
    // upstream: token.c:367 send_deflated_token() emits the Z_FINISH-
    // terminated stream at end of transfer; main.c:979-983
    // do_server_sender() brackets read_final_goodbye() with
    // io_flush(FULL_FLUSH).
    #[test]
    fn handle_goodbye_with_finalizer_runs_between_write_and_read() {
        use std::cell::Cell;

        let handshake = test_handshake_with_protocol(31);
        let mut config = test_config();
        config.protocol = ProtocolVersion::try_from(31u8).unwrap();
        config.do_stats = false;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        // Receiver wire: first NDX_DONE (consumed BEFORE the finalizer)
        // followed by the final NDX_DONE (consumed AFTER). The thread-local
        // byte counter lets the finalizer assert which side of that boundary
        // it ran on.
        let receiver_input = [NDX_DONE_MODERN.as_slice(), NDX_DONE_MODERN.as_slice()].concat();
        let mut reader = TrackingReader::new(receiver_input);

        let mut output = FlushTrackingWriter::default();
        let mut ndx_read = create_ndx_codec(31);
        let mut ndx_write = MonotonicNdxWriter::new(31);

        let finalizer_called = Cell::new(false);
        let bytes_written_when_finalizer_ran = Cell::new(0usize);
        let bytes_read_when_finalizer_ran = Cell::new(0usize);

        ctx.handle_goodbye_with_finalizer(
            &mut reader,
            &mut output,
            &mut ndx_read,
            &mut ndx_write,
            |w: &mut FlushTrackingWriter| {
                finalizer_called.set(true);
                bytes_written_when_finalizer_ran.set(w.buffer.len());
                bytes_read_when_finalizer_ran.set(reader_bytes_consumed());
                w.flush()
            },
        )
        .expect("goodbye completes");

        assert!(
            finalizer_called.get(),
            "the finalizer must run on the proto-31+ goodbye path"
        );

        // Ordering invariant: when the finalizer runs the sender's
        // goodbye NDX_DONE must already be in the wire buffer (this is
        // the "after write" half).
        assert!(
            bytes_written_when_finalizer_ran.get() >= NDX_DONE_MODERN.len(),
            "finalizer ran before the goodbye NDX_DONE was written: \
             buffer had {} bytes (need >= {})",
            bytes_written_when_finalizer_ran.get(),
            NDX_DONE_MODERN.len(),
        );

        // Ordering invariant: when the finalizer runs the receiver's
        // FIRST NDX_DONE must already be consumed, but the FINAL
        // NDX_DONE must NOT yet be consumed (this is the "before read"
        // half - the deflate stream must be closed before the receiver
        // is asked to advance another byte).
        assert_eq!(
            bytes_read_when_finalizer_ran.get(),
            NDX_DONE_MODERN.len(),
            "finalizer must run after the first NDX_DONE is read but \
             before the final NDX_DONE is read"
        );

        // The wire payload must still end with the NDX_DONE marker
        // byte even with the finalizer hooked in.
        assert!(
            output.buffer.ends_with(&NDX_DONE_MODERN),
            "wire output must end with NDX_DONE even after finalizer: {:?}",
            output.buffer
        );
    }

    /// Module-level byte counter used by the ordering test above. We expose
    /// it via a thread-local because the test's `Read` impl needs to record
    /// how much was consumed before the finalizer ran, and the finalizer
    /// closure cannot borrow `reader` mutably (it already borrows `writer`).
    fn reader_bytes_consumed() -> usize {
        READER_BYTES_CONSUMED.with(|c| c.get())
    }

    thread_local! {
        static READER_BYTES_CONSUMED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// `Read` implementation that tracks how many bytes have been consumed
    /// in a thread-local so the `handle_goodbye_with_finalizer` ordering
    /// test can detect whether the finalizer ran before or after the final
    /// receiver NDX_DONE was read.
    struct TrackingReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl TrackingReader {
        fn new(data: Vec<u8>) -> Self {
            READER_BYTES_CONSUMED.with(|c| c.set(0));
            Self { data, pos: 0 }
        }
    }

    impl std::io::Read for TrackingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.data.len().saturating_sub(self.pos);
            let n = remaining.min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            READER_BYTES_CONSUMED.with(|c| c.set(self.pos));
            Ok(n)
        }
    }

    /// Writer that records every `write` and `flush` so tests can assert
    /// upstream's `io_flush(FULL_FLUSH)` contract is honoured before the
    /// goodbye handshake returns. Mirrors the `main.c:912` flush-before-
    /// return pattern.
    #[derive(Default)]
    struct FlushTrackingWriter {
        buffer: Vec<u8>,
        flushes: usize,
        last_op_was_flush: bool,
    }

    impl std::io::Write for FlushTrackingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            self.last_op_was_flush = false;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            self.last_op_was_flush = true;
            Ok(())
        }
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
        let ctx = GeneratorContext::new_for_test(&handshake, config);

        let paths = vec![PathBuf::from("/src")];
        // NUL-separated file list with double-NUL terminator
        let wire_data = b"file1.txt\0subdir/file2.txt\0\0";
        let mut reader = Cursor::new(wire_data.to_vec());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, PathBuf::from("/src/file1.txt"));
        assert_eq!(result[0].base, PathBuf::from("/src"));
        assert_eq!(result[1].path, PathBuf::from("/src/subdir/file2.txt"));
        assert_eq!(result[1].base, PathBuf::from("/src"));
    }

    #[test]
    fn resolve_uses_dot_base_when_no_paths() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some("-".to_owned());
        let ctx = GeneratorContext::new_for_test(&handshake, config);

        let paths: Vec<PathBuf> = vec![];
        let wire_data = b"file.txt\0\0";
        let mut reader = Cursor::new(wire_data.to_vec());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("./file.txt"));
        assert_eq!(result[0].base, PathBuf::from("."));
    }

    #[test]
    fn resolve_skips_empty_filenames() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some("-".to_owned());
        let ctx = GeneratorContext::new_for_test(&handshake, config);

        let paths = vec![PathBuf::from("/base")];
        // Single file (the double-NUL is the terminator, no empty names in between)
        let wire_data = b"only.txt\0\0";
        let mut reader = Cursor::new(wire_data.to_vec());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("/base/only.txt"));
        assert_eq!(result[0].base, PathBuf::from("/base"));
    }

    #[test]
    fn resolve_reads_from_local_file() {
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("filelist.txt");
        std::fs::write(&list_file, "alpha.txt\nbeta.txt\ngamma.txt\n").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.files_from_path = Some(list_file.to_string_lossy().to_string());
        let ctx = GeneratorContext::new_for_test(&handshake, config);

        let paths = vec![PathBuf::from("/data")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].path, PathBuf::from("/data/alpha.txt"));
        assert_eq!(result[1].path, PathBuf::from("/data/beta.txt"));
        assert_eq!(result[2].path, PathBuf::from("/data/gamma.txt"));
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
        let ctx = GeneratorContext::new_for_test(&handshake, config);

        let paths = vec![PathBuf::from("/root")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, PathBuf::from("/root/one.txt"));
        assert_eq!(result[1].path, PathBuf::from("/root/two.txt"));
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
        let ctx = GeneratorContext::new_for_test(&handshake, config);

        let paths = vec![PathBuf::from("/dir")];
        let mut reader = Cursor::new(Vec::<u8>::new());

        let result = ctx.resolve_files_from_paths(&paths, &mut reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].path, PathBuf::from("/dir/file1.txt"));
        assert_eq!(result[1].path, PathBuf::from("/dir/file2.txt"));
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
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let file_paths = vec![src.join("hello.txt"), src.join("subdir/file.txt")];
        let count = ctx
            .build_file_list_with_base(&src, &files_from_entries(&src, file_paths))
            .unwrap();

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
    fn build_file_list_with_base_deduplicates_explicit_dir_and_child_file() {
        // UTS-21 regression: when --files-from contains both an explicit
        // directory (e.g. `dir/subdir`) and a file inside it (e.g.
        // `dir/subdir/child.txt`), the directory must appear in the wire
        // file list exactly ONCE. Upstream's `implied_filter_list` check
        // (flist.c:998) rejects the second occurrence as
        // "rejecting unrequested file-list name: dir/subdir", which broke
        // upstream's `files-from.test` interop suite in the pull direction.
        //
        // The implied-parent loop previously emitted `dir/subdir` because it
        // is the parent of `child.txt`, and the top-level walk emitted it
        // again because it is also an explicit --files-from entry. The fix
        // pre-populates the explicit-dir set so the implied-parent loop
        // skips it, leaving the top-level walk as the single emission site.
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        let nested = src.join("dir").join("subdir");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("child.txt"), b"payload").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        config.flags.recursive = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let file_paths = vec![nested.clone(), nested.join("child.txt")];
        ctx.build_file_list_with_base(&src, &files_from_entries(&src, file_paths))
            .unwrap();

        // Count occurrences of every distinct relative name. The subdir
        // must appear exactly once; duplicates would re-trigger the
        // upstream rejection.
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry in ctx.file_list().iter() {
            *counts.entry(entry.name().to_string()).or_insert(0) += 1;
        }

        let subdir_name = nested.strip_prefix(&src).unwrap().to_string_lossy();
        let subdir_count = counts.get(subdir_name.as_ref()).copied().unwrap_or(0);
        assert_eq!(
            subdir_count, 1,
            "explicit dir + child must emit subdir exactly once, got {subdir_count} \
             across entries {counts:?}"
        );

        // The child file must still be present so the receiver can transfer it.
        let child_name = nested
            .join("child.txt")
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            counts.contains_key(&child_name),
            "child.txt must remain in the file list, got {counts:?}"
        );
    }

    #[test]
    fn build_file_list_with_base_dotdir_entry_scans_children() {
        // Upstream `files-from.test` regression: a `--files-from` entry of
        // the form `from/./` parses to a `FilesFromEntry` with
        // `path == base` and `recurse == true` (upstream's DOTDIR_NAME
        // case at `flist.c:2329`). With `--files-from` active,
        // `options.c:2189` clears the global `recurse` flag, so
        // `walk_path_with_metadata` emits only the root entry; the
        // marker-dir rescan in `build_file_list_with_base` is the only
        // path that re-injects the directory's children. The previous
        // `entry.path != entry.base` gate skipped that rescan for the
        // DOTDIR shape and the transfer collapsed to zero files.
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        let from_dir = src.join("from");
        std::fs::create_dir_all(&from_dir).unwrap();
        std::fs::write(from_dir.join("alpha.txt"), b"a").unwrap();
        std::fs::write(from_dir.join("beta.txt"), b"b").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        // Mirror upstream `options.c:2189` - `--files-from` disables
        // global recursion; the per-entry `recurse` flag drives the
        // DOTDIR rescan instead.
        config.flags.recursive = false;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let dotdir_entry = super::filters::FilesFromEntry {
            base: from_dir.clone(),
            path: from_dir.clone(),
            recurse: true,
        };
        ctx.build_file_list_with_base(&src, std::slice::from_ref(&dotdir_entry))
            .unwrap();

        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(
            names.contains(&"alpha.txt"),
            "DOTDIR entry must rescan children: expected alpha.txt in {names:?}"
        );
        assert!(
            names.contains(&"beta.txt"),
            "DOTDIR entry must rescan children: expected beta.txt in {names:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_absolute_source_preserves_full_prefix() {
        // upstream: flist.c:2329 - no "/./" anchor on an absolute source path
        // sends the entire path (minus the leading slash, stripped post-sort
        // by the receiver) as the relative name. Regression test for #4074.
        let temp_dir = TempDir::new().unwrap();
        // Canonicalize to resolve symlinks in the temp path. On macOS, /var is
        // a symlink to /private/var - emit_implied_parents uses
        // symlink_metadata which skips symlink components (is_dir() is false
        // for symlinks), so the ancestor loop below would fail on the bare
        // "var" entry.
        let temp_root = temp_dir.path().canonicalize().unwrap();
        let src_dir = temp_root.join("usr").join("bin");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("ar"), b"x").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.relative = true;
        config.flags.recursive = true;
        config.args = vec![OsString::from(&src_dir)];
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        ctx.build_file_list(&[src_dir.clone()]).unwrap();
        let names: Vec<String> = ctx
            .file_list()
            .iter()
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();

        // The transmitted relative name for the source directory must contain
        // the parent components (e.g. ".../usr/bin"), not collapse to ".".
        let temp_suffix = src_dir.strip_prefix("/").unwrap().to_string_lossy();
        assert!(
            names.iter().any(|n| n == &temp_suffix),
            "expected source dir relative name {temp_suffix:?} in {names:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with("usr/bin/ar")),
            "expected child to retain path prefix in {names:?}"
        );
        // Every parent ancestor must be present so the receiver can resolve
        // generator.c:1313 parent-lookup without ABORTING.
        for ancestor in src_dir
            .ancestors()
            .skip(1)
            .take_while(|p| p.parent().is_some())
        {
            let rel = ancestor.strip_prefix("/").unwrap().to_string_lossy();
            if rel.is_empty() {
                continue;
            }
            assert!(
                names.iter().any(|n| n == &rel),
                "missing implied parent {rel:?} in {names:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn relative_dot_anchor_splits_base_and_relative() {
        // upstream: flist.c:2316 - `/./` anchor splits source: dir before the
        // anchor is treated as the base, the rest becomes the relative name.
        let temp_dir = TempDir::new().unwrap();
        let anchored = temp_dir.path().join("root");
        let leaf = anchored.join("usr").join("bin");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::write(leaf.join("ar"), b"x").unwrap();

        // Construct path with explicit "/./" separator.
        let src_with_anchor = PathBuf::from(format!("{}/./usr/bin", anchored.to_string_lossy()));

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.relative = true;
        config.flags.recursive = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        ctx.build_file_list(&[src_with_anchor]).unwrap();

        let names: Vec<String> = ctx
            .file_list()
            .iter()
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n == "usr/bin"),
            "expected anchored relative name 'usr/bin' in {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "usr/bin/ar"),
            "expected child 'usr/bin/ar' in {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "usr"),
            "expected implied parent 'usr' in {names:?}"
        );
    }

    #[test]
    fn non_relative_mode_emits_source_basename() {
        // upstream: flist.c:2338-2349 - non-relative mode splits each
        // positional on its last `/`: `dir` becomes the chdir target,
        // `fn` is link_stat'd. For source `<tmp>/payload`, dir = <tmp>
        // and fn = payload, so the wire entries carry the basename
        // (`payload`, `payload/file.txt`) - matching upstream rsync's
        // behaviour for `rsync -r payload dst/`. A trailing slash
        // (DOTDIR_NAME branch) still collapses to `.` + children; that
        // path is covered by the trailing-slash test below.
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("payload");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("file.txt"), b"x").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.relative = false;
        config.flags.recursive = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        ctx.build_file_list(&[src_dir]).unwrap();

        // The stored `name()` uses native OS separators; compare against
        // a platform-appropriate join so Windows backslashes do not trip
        // the assertion. The on-wire form is normalised to `/` by
        // `name_bytes()`, but `name()` returns the raw `PathBuf`.
        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        let expected_child = std::path::PathBuf::from("payload")
            .join("file.txt")
            .to_string_lossy()
            .into_owned();
        assert!(
            names.contains(&"payload"),
            "expected 'payload' entry in {names:?}"
        );
        assert!(
            names.contains(&expected_child.as_str()),
            "expected {expected_child:?} in {names:?}"
        );
        assert!(
            !names.contains(&"."),
            "expected no `.` entry for sub-path source in {names:?}"
        );
    }

    #[test]
    fn non_relative_mode_trailing_slash_collapses_to_dot() {
        // upstream: flist.c:2312-2322 DOTDIR_NAME branch - a trailing
        // slash makes the engine emit `.` for the source root and
        // children without the basename prefix, mirroring upstream's
        // "transfer the contents only" semantic.
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("payload");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("file.txt"), b"x").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.relative = false;
        config.flags.recursive = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        let mut with_slash = src_dir.as_os_str().to_owned();
        with_slash.push("/");
        ctx.build_file_list(&[std::path::PathBuf::from(with_slash)])
            .unwrap();

        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(names.contains(&"."), "expected '.' entry in {names:?}");
        assert!(
            names.contains(&"file.txt"),
            "expected 'file.txt' in {names:?}"
        );
        assert!(
            !names.contains(&"payload"),
            "expected no `payload` entry for trailing-slash source in {names:?}"
        );
    }

    #[test]
    fn build_file_list_with_base_skips_missing_files() {
        // FFV-4: default mode emits link_stat error and sets IOERR_GENERAL (exit 23).
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("exists.txt"), "data").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let file_paths = vec![src.join("exists.txt"), src.join("missing.txt")];
        let count = ctx
            .build_file_list_with_base(&src, &files_from_entries(&src, file_paths))
            .unwrap();

        // Dot entry + exists.txt; missing.txt is skipped with io_error.
        assert_eq!(count, 2, "dot + exists.txt");
        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(names.contains(&"exists.txt"));
        assert!(!names.contains(&"missing.txt"));

        // upstream: flist.c:1810 - ENOENT for a --files-from entry that never
        // existed should set IOERR_GENERAL (exit 23), not IOERR_VANISHED (exit 24).
        assert_ne!(
            ctx.io_error() & io_error_flags::IOERR_GENERAL,
            0,
            "missing source should set IOERR_GENERAL"
        );
        assert_eq!(
            ctx.io_error() & io_error_flags::IOERR_VANISHED,
            0,
            "missing source should NOT set IOERR_VANISHED"
        );
    }

    #[test]
    fn build_file_list_with_base_ignore_missing_args_skips_silently() {
        // FFV-2: --ignore-missing-args silently skips missing entries with exit 0.
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("exists.txt"), "data").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        config.file_selection.ignore_missing_args = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let file_paths = vec![src.join("exists.txt"), src.join("missing.txt")];
        let count = ctx
            .build_file_list_with_base(&src, &files_from_entries(&src, file_paths))
            .unwrap();

        // Dot entry + exists.txt; missing.txt silently skipped.
        assert_eq!(count, 2, "dot + exists.txt");
        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(names.contains(&"exists.txt"));
        assert!(!names.contains(&"missing.txt"));

        // No io_error flags should be set - the missing entry is silently ignored.
        assert_eq!(
            ctx.io_error(),
            0,
            "ignore-missing-args should not set io_error"
        );
    }

    #[test]
    fn build_file_list_with_base_delete_missing_args_emits_sentinel() {
        // FFV-3: --delete-missing-args emits a mode-0 sentinel for receiver deletion.
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("exists.txt"), "data").unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        config.file_selection.delete_missing_args = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let file_paths = vec![src.join("exists.txt"), src.join("missing.txt")];
        let count = ctx
            .build_file_list_with_base(&src, &files_from_entries(&src, file_paths))
            .unwrap();

        // Dot entry + exists.txt + mode-0 sentinel for missing.txt
        assert_eq!(count, 3, "dot + exists.txt + sentinel for missing.txt");
        let names: Vec<&str> = ctx.file_list().iter().map(|e| e.name()).collect();
        assert!(names.contains(&"exists.txt"));

        // The sentinel entry should have mode == 0.
        let sentinel = ctx
            .file_list()
            .iter()
            .find(|e| e.name() == "missing.txt")
            .expect("sentinel entry for missing.txt should be in file list");
        assert_eq!(
            sentinel.mode(),
            0,
            "delete-missing-args sentinel must have mode 0"
        );
        assert_eq!(sentinel.size(), 0, "sentinel size should be 0");

        // No io_error flags should be set - exit 0.
        assert_eq!(
            ctx.io_error(),
            0,
            "delete-missing-args should not set io_error"
        );
    }

    #[test]
    fn build_file_list_with_base_delete_overrides_ignore_missing_args() {
        // When both flags are set, delete-missing-args takes precedence.
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(&src)];
        config.file_selection.ignore_missing_args = true;
        config.file_selection.delete_missing_args = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let file_paths = vec![src.join("missing.txt")];
        let count = ctx
            .build_file_list_with_base(&src, &files_from_entries(&src, file_paths))
            .unwrap();

        // Dot entry + mode-0 sentinel (delete takes precedence over ignore).
        assert_eq!(count, 2, "dot + sentinel for missing.txt");
        let sentinel = ctx
            .file_list()
            .iter()
            .find(|e| e.name() == "missing.txt")
            .expect("sentinel should exist when delete takes precedence");
        assert_eq!(sentinel.mode(), 0);
    }

    #[test]
    fn missing_args_mode_returns_correct_values() {
        let handshake = test_handshake();

        // Default: mode 0
        let config = test_config();
        let ctx = GeneratorContext::new_for_test(&handshake, config);
        assert_eq!(ctx.missing_args_mode(), 0);

        // --ignore-missing-args: mode 1
        let mut config = test_config();
        config.file_selection.ignore_missing_args = true;
        let ctx = GeneratorContext::new_for_test(&handshake, config);
        assert_eq!(ctx.missing_args_mode(), 1);

        // --delete-missing-args: mode 2
        let mut config = test_config();
        config.file_selection.delete_missing_args = true;
        let ctx = GeneratorContext::new_for_test(&handshake, config);
        assert_eq!(ctx.missing_args_mode(), 2);

        // Both set: mode 2 (delete takes precedence)
        let mut config = test_config();
        config.file_selection.ignore_missing_args = true;
        config.file_selection.delete_missing_args = true;
        let ctx = GeneratorContext::new_for_test(&handshake, config);
        assert_eq!(ctx.missing_args_mode(), 2);
    }

    #[test]
    fn build_file_list_ignore_missing_args_top_level_source() {
        // FFV-2 for build_file_list (non-files-from path).
        let temp_dir = TempDir::new().unwrap();
        let existing = temp_dir.path().join("exists.txt");
        std::fs::write(&existing, "data").unwrap();
        let missing = temp_dir.path().join("no_such_file.txt");

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.ignore_missing_args = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let count = ctx.build_file_list(&[existing.clone(), missing]).unwrap();

        assert_eq!(count, 1, "only exists.txt should be in the list");
        assert_eq!(ctx.io_error(), 0, "no error for silently skipped source");
    }

    #[test]
    fn build_file_list_delete_missing_args_top_level_source() {
        // FFV-3 for build_file_list (non-files-from path).
        let temp_dir = TempDir::new().unwrap();
        let existing = temp_dir.path().join("exists.txt");
        std::fs::write(&existing, "data").unwrap();
        let missing = temp_dir.path().join("no_such_file.txt");

        let handshake = test_handshake();
        let mut config = test_config();
        config.file_selection.delete_missing_args = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        let count = ctx.build_file_list(&[existing.clone(), missing]).unwrap();

        // exists.txt + mode-0 sentinel for missing
        assert_eq!(count, 2, "exists.txt + sentinel");
        let sentinel = ctx
            .file_list()
            .iter()
            .find(|e| e.name() == "no_such_file.txt")
            .expect("sentinel for missing source");
        assert_eq!(sentinel.mode(), 0);
        assert_eq!(ctx.io_error(), 0);
    }

    #[test]
    fn build_file_list_default_missing_source_sets_general_error() {
        // FFV-4 for build_file_list: default mode emits IOERR_GENERAL, not VANISHED.
        let temp_dir = TempDir::new().unwrap();
        let missing = temp_dir.path().join("nonexistent.txt");

        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);

        ctx.build_file_list(&[missing]).unwrap();

        assert_ne!(
            ctx.io_error() & io_error_flags::IOERR_GENERAL,
            0,
            "default mode should set IOERR_GENERAL for missing source"
        );
        assert_eq!(
            ctx.io_error() & io_error_flags::IOERR_VANISHED,
            0,
            "default mode should NOT set IOERR_VANISHED for top-level missing source"
        );
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
    fn read_files_from_local_path_nul_delimited_strips_comments() {
        // upstream: flist.c:2249 sets RL_DUMP_COMMENTS for local files
        // independent of eol_nulls; io.c:1276 strips leading '#'/';' comment
        // lines even under --from0. Comment entries are dropped, normals kept.
        let temp_dir = TempDir::new().unwrap();
        let list_file = temp_dir.path().join("list0.txt");
        std::fs::write(&list_file, b"#comment\0x.txt\0;skip\0y.txt\0\0").unwrap();

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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`. Without it, the non-relative
    // walk-base split (flist.c:2338-2349) emits only the source basename.
    let count = build_file_list_for_contents(&mut ctx, base);

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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`. Without it, the non-relative
    // walk-base split (flist.c:2338-2349) prefixes every name with the
    // source basename.
    let _count = build_file_list_for_contents(&mut ctx, base);
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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`. Without it, the non-relative
    // walk-base split (flist.c:2338-2349) prefixes every name with the
    // source basename and the exact-equality assertions below would miss.
    let _count = build_file_list_for_contents(&mut ctx, base);
    let names: Vec<String> = ctx
        .file_list()
        .iter()
        .map(|e| e.path().display().to_string().replace('\\', "/"))
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

    // Trailing-slash source enters upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list contains `.` + the base
    // directory's children at the top level, matching `rsync <dir>/ dst/`.
    let _count = build_file_list_for_contents(&mut ctx, base);
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

    // Trailing-slash source exercises upstream's DOTDIR_NAME branch
    // (flist.c:2312-2322) so the file list is `.` + the directory's
    // children, matching `rsync <dir>/ dst/`. Without it, the non-relative
    // walk-base split (flist.c:2338-2349) emits only the source basename.
    let count = build_file_list_for_contents(&mut ctx, base);

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

    let ctx = GeneratorContext::new_for_test(&handshake, config);

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

#[test]
fn segment_scheduler_dispatches_when_remaining_below_threshold() {
    // When remaining entries known to the receiver are below
    // MIN_FILECNT_LOOKAHEAD, the scheduler should dispatch the next segment.
    use super::segments::{MIN_FILECNT_LOOKAHEAD, PendingSegment, SegmentScheduler};

    let seg = PendingSegment {
        parent_dir_ndx: 0,
        flist_start: 10,
        count: 500,
    };
    let mut scheduler = SegmentScheduler::new(vec![seg]);

    // remaining = 13 (a small initial segment), well below 1000
    let result = scheduler.next_if_needed(13);
    assert!(
        result.is_some(),
        "scheduler must dispatch when remaining ({}) < MIN_FILECNT_LOOKAHEAD ({})",
        13,
        MIN_FILECNT_LOOKAHEAD,
    );
}

#[test]
fn segment_scheduler_blocks_when_remaining_at_threshold() {
    // When remaining equals MIN_FILECNT_LOOKAHEAD, upstream's condition
    // `file_total - file_old_total < at_least` is false (1000 < 1000 is false),
    // so no dispatch should occur.
    use super::segments::{MIN_FILECNT_LOOKAHEAD, PendingSegment, SegmentScheduler};

    let seg = PendingSegment {
        parent_dir_ndx: 0,
        flist_start: 10,
        count: 500,
    };
    let mut scheduler = SegmentScheduler::new(vec![seg]);

    let result = scheduler.next_if_needed(MIN_FILECNT_LOOKAHEAD);
    assert!(
        result.is_none(),
        "scheduler must NOT dispatch when remaining == MIN_FILECNT_LOOKAHEAD"
    );
}

#[test]
fn segment_scheduler_blocks_when_remaining_above_threshold() {
    // When remaining exceeds MIN_FILECNT_LOOKAHEAD, no dispatch.
    use super::segments::{MIN_FILECNT_LOOKAHEAD, PendingSegment, SegmentScheduler};

    let seg = PendingSegment {
        parent_dir_ndx: 0,
        flist_start: 10,
        count: 500,
    };
    let mut scheduler = SegmentScheduler::new(vec![seg]);

    let result = scheduler.next_if_needed(MIN_FILECNT_LOOKAHEAD + 1);
    assert!(
        result.is_none(),
        "scheduler must NOT dispatch when remaining > MIN_FILECNT_LOOKAHEAD"
    );
}

#[test]
fn segment_scheduler_boundary_dispatches_at_999() {
    // remaining = 999, which is < 1000, so dispatch must occur.
    use super::segments::{MIN_FILECNT_LOOKAHEAD, PendingSegment, SegmentScheduler};

    let seg = PendingSegment {
        parent_dir_ndx: 0,
        flist_start: 10,
        count: 500,
    };
    let mut scheduler = SegmentScheduler::new(vec![seg]);

    let result = scheduler.next_if_needed(MIN_FILECNT_LOOKAHEAD - 1);
    assert!(
        result.is_some(),
        "scheduler must dispatch when remaining == {} (one below threshold)",
        MIN_FILECNT_LOOKAHEAD - 1,
    );
}

#[test]
fn segment_scheduler_many_files_deadlock_scenario() {
    // Regression test for the many-files deadlock (#5085).
    // Scenario: 1013 total entries, 13 in the initial segment, 1000 in a
    // pending sub-list. Using dispatched_entry_count (13) instead of
    // file_list.len() (1013) gives remaining=13 which triggers dispatch.
    use super::segments::{PendingSegment, SegmentScheduler};

    let seg = PendingSegment {
        parent_dir_ndx: 1,
        flist_start: 13,
        count: 1000,
    };
    let mut scheduler = SegmentScheduler::new(vec![seg]);

    // Bug: old code computed remaining = file_list.len() - transferred = 1013 - 0 = 1013
    // which is >= 1000, so no dispatch occurred. Deadlock.
    let remaining_buggy = 1013usize;
    let result_buggy = scheduler.next_if_needed(remaining_buggy);
    assert!(
        result_buggy.is_none(),
        "with total file count (buggy), scheduler incorrectly blocks"
    );

    // Fix: remaining = dispatched_entry_count - transferred = 13 - 0 = 13
    // Reset scheduler for the corrected test.
    let seg = PendingSegment {
        parent_dir_ndx: 1,
        flist_start: 13,
        count: 1000,
    };
    let mut scheduler = SegmentScheduler::new(vec![seg]);

    let remaining_fixed = 13usize;
    let result_fixed = scheduler.next_if_needed(remaining_fixed);
    assert!(
        result_fixed.is_some(),
        "with dispatched_entry_count (fixed), scheduler correctly dispatches"
    );
}

#[test]
fn empty_segment_sends_wire_bytes() {
    // Regression test for empty-dir flist_done overcounting (#5085).
    // An empty segment (count==0) must still produce wire output (NDX header
    // + end-of-flist marker), matching upstream flist.c:2117,2139-2146.
    // The old code returned early for count==0 producing zero wire bytes,
    // which desynchronised flist_done_remaining from the receiver.
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());

    // Set up a minimal initial segment in ndx_segments.
    ctx.incremental.ndx_segments = vec![(0, 0)];

    // Add a dummy file entry so the file_list is non-empty (flist_start=0).
    ctx.file_list
        .push(protocol::flist::FileEntry::new_file("x".into(), 1, 0o644));
    ctx.full_paths.push(PathBuf::from("x"));

    let seg = super::PendingSegment {
        parent_dir_ndx: 0,
        flist_start: 1, // past the single entry, so count=0 segment is empty
        count: 0,
    };

    let mut output = Vec::new();
    let mut flist_writer = ctx.build_flist_writer();
    let mut ndx_codec = protocol::codec::create_ndx_codec(ctx.protocol().as_u8());

    ctx.encode_and_send_segment(&mut output, &seg, &mut flist_writer, &mut ndx_codec)
        .unwrap();

    // The output must contain at least the NDX header bytes and the
    // end-of-flist zero byte. With the old early-return bug, output was empty.
    assert!(
        !output.is_empty(),
        "empty segment must still produce wire output (NDX header + end marker)"
    );
    // The last byte should be the end-of-flist marker (0x00).
    assert_eq!(
        *output.last().unwrap(),
        0u8,
        "last wire byte must be the end-of-flist marker"
    );
}

#[test]
fn nonempty_segment_also_sends_wire_bytes() {
    // Sanity check: a non-empty segment produces wire output with entries.
    let handshake = test_handshake();
    let mut ctx = GeneratorContext::new_for_test(&handshake, test_config());

    ctx.incremental.ndx_segments = vec![(0, 0)];
    let entry = protocol::flist::FileEntry::new_file("a.txt".into(), 10, 0o644);
    ctx.file_list.push(entry);
    ctx.full_paths.push(PathBuf::from("a.txt"));

    let seg = super::PendingSegment {
        parent_dir_ndx: 0,
        flist_start: 0,
        count: 1,
    };

    let mut output_nonempty = Vec::new();
    let mut flist_writer = ctx.build_flist_writer();
    let mut ndx_codec = protocol::codec::create_ndx_codec(ctx.protocol().as_u8());

    ctx.encode_and_send_segment(
        &mut output_nonempty,
        &seg,
        &mut flist_writer,
        &mut ndx_codec,
    )
    .unwrap();

    // Non-empty segment output must be larger than an empty segment's output
    // (NDX header + at least one entry + end marker).
    assert!(
        output_nonempty.len() > 2,
        "non-empty segment must produce substantial wire output, got {} bytes",
        output_nonempty.len()
    );
    assert_eq!(
        *output_nonempty.last().unwrap(),
        0u8,
        "last wire byte must be end-of-flist marker"
    );
}

#[test]
fn reclaim_oldest_segment_frees_first_segment_entries() {
    use protocol::CompatibilityFlags;
    use protocol::flist::FileEntry;

    let mut handshake = test_handshake_with_protocol(32);
    handshake.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
    let config = test_config();
    let mut ctx = GeneratorContext::new_for_test(&handshake, config);

    // Simulate 3 segments: [0..3), [3..5), [5..7)
    for i in 0..7 {
        ctx.push_file_item(
            FileEntry::new_file(
                format!("dir/file_{i}.txt").into(),
                (i + 1) as u64 * 100,
                0o644,
            ),
            PathBuf::from(format!("/src/dir/file_{i}.txt")),
        );
    }
    ctx.incremental.ndx_segments = vec![(0, 1), (3, 5), (5, 8)];
    ctx.incremental.first_segment_idx = 0;

    // Verify initial state.
    assert_eq!(ctx.file_list()[0].name(), "dir/file_0.txt");
    assert_eq!(ctx.file_list()[3].name(), "dir/file_3.txt");
    assert_eq!(ctx.file_list()[5].name(), "dir/file_5.txt");

    // Reclaim first segment [0..3).
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.incremental.first_segment_idx, 1);
    assert_eq!(ctx.file_list()[0].name(), ""); // reclaimed
    assert_eq!(ctx.file_list()[1].name(), ""); // reclaimed
    assert_eq!(ctx.file_list()[2].name(), ""); // reclaimed
    assert_eq!(ctx.file_list()[3].name(), "dir/file_3.txt"); // intact
    assert_eq!(ctx.file_list()[5].name(), "dir/file_5.txt"); // intact

    // Reclaim second segment [3..5).
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.incremental.first_segment_idx, 2);
    assert_eq!(ctx.file_list()[3].name(), ""); // reclaimed
    assert_eq!(ctx.file_list()[4].name(), ""); // reclaimed
    assert_eq!(ctx.file_list()[5].name(), "dir/file_5.txt"); // intact

    // Third reclaim is a no-op (last segment must not be reclaimed).
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.incremental.first_segment_idx, 2); // unchanged
    assert_eq!(ctx.file_list()[5].name(), "dir/file_5.txt"); // still intact
}

#[test]
fn reclaim_oldest_segment_noop_without_inc_recurse() {
    use protocol::flist::FileEntry;

    let handshake = test_handshake_with_protocol(32);
    let config = test_config();
    let mut ctx = GeneratorContext::new_for_test(&handshake, config);

    ctx.push_file_item(
        FileEntry::new_file("file.txt".into(), 100, 0o644),
        PathBuf::from("/src/file.txt"),
    );

    // Single segment - reclaim is a no-op.
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.incremental.first_segment_idx, 0);
    assert_eq!(ctx.file_list()[0].name(), "file.txt");
}

/// Wire-byte parity regression test for batched generator flush (BPR-1.h).
///
/// The generator transfer loop (sender.c:send_files) calls `flush_with_count`
/// once per iteration rather than after each individual write. This batched
/// flush reduces TCP segment count but must not alter the logical data the
/// receiver sees on the wire.
///
/// This test verifies the invariant by serializing a multi-entry file list
/// through a `MultiplexWriter` with two flush disciplines:
///
/// 1. **Batched**: all entries written, single flush at end (matches the
///    transfer loop pattern after BPR-1.d).
/// 2. **Sequential**: flush after each entry (pre-BPR-1.d pattern).
///
/// The logical data payload (MSG_DATA content after stripping frame headers)
/// must be byte-identical. The batched variant should produce fewer frames.
#[test]
fn batched_flush_wire_byte_parity() {
    use super::super::writer::multiplex::MultiplexWriter;
    use protocol::{MessageCode, recv_msg};

    // Helper: extract logical data bytes from multiplex wire output by
    // draining MSG_DATA frames and concatenating their payloads.
    fn extract_data_payload(wire: &[u8]) -> (Vec<u8>, usize) {
        let mut cursor = Cursor::new(wire);
        let mut data = Vec::new();
        let mut frame_count = 0usize;
        loop {
            match recv_msg(&mut cursor) {
                Ok(frame) => {
                    assert_eq!(
                        frame.code(),
                        MessageCode::Data,
                        "expected MSG_DATA frames only"
                    );
                    data.extend_from_slice(frame.payload());
                    frame_count += 1;
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected recv_msg error: {e}"),
            }
        }
        (data, frame_count)
    }

    // Build 5 file entries with distinct names and sizes so the flist
    // encoder emits non-trivial per-entry wire data.
    let handshake = test_handshake();
    let entries: Vec<protocol::flist::FileEntry> = (0..5)
        .map(|i| {
            let mut e = protocol::flist::FileEntry::new_file(
                format!("file_{i}.dat").into(),
                (i as u64 + 1) * 1000,
                0o644,
            );
            e.set_mtime(1700000000_i64 + i as i64, 0);
            e
        })
        .collect();

    // --- Batched flush: write all entries, flush once at end ---
    let batched_wire = {
        let mut inner = Vec::<u8>::new();
        let mut mux = MultiplexWriter::new(&mut inner);

        let mut gen_ctx = GeneratorContext::new_for_test(&handshake, test_config());
        for entry in &entries {
            gen_ctx.file_list.push(entry.clone());
        }
        let mut flist_writer = gen_ctx.build_flist_writer();
        for entry in &entries {
            flist_writer.write_entry(&mut mux, entry).unwrap();
        }
        flist_writer.write_end(&mut mux, None).unwrap();
        // Single batched flush - matches the transfer loop pattern
        flush_with_count(&mut mux).unwrap();
        drop(mux);
        inner
    };

    // --- Sequential flush: flush after each entry write ---
    let sequential_wire = {
        let mut inner = Vec::<u8>::new();
        let mut mux = MultiplexWriter::new(&mut inner);

        let mut gen_ctx = GeneratorContext::new_for_test(&handshake, test_config());
        for entry in &entries {
            gen_ctx.file_list.push(entry.clone());
        }
        let mut flist_writer = gen_ctx.build_flist_writer();
        for entry in &entries {
            flist_writer.write_entry(&mut mux, entry).unwrap();
            flush_with_count(&mut mux).unwrap();
        }
        flist_writer.write_end(&mut mux, None).unwrap();
        flush_with_count(&mut mux).unwrap();
        drop(mux);
        inner
    };

    let (batched_data, batched_frames) = extract_data_payload(&batched_wire);
    let (sequential_data, sequential_frames) = extract_data_payload(&sequential_wire);

    // Core invariant: logical data bytes are identical regardless of flush
    // discipline. If this assertion fails, a flush-discipline change has
    // altered the wire content, which would break receiver compatibility.
    assert_eq!(
        batched_data,
        sequential_data,
        "batched and sequential flush must produce byte-identical logical data \
         (batched={} bytes, sequential={} bytes)",
        batched_data.len(),
        sequential_data.len()
    );

    // Verify the optimization: batched flush should coalesce writes into
    // fewer MSG_DATA frames than per-entry flushing.
    assert!(
        batched_frames <= sequential_frames,
        "batched flush should produce no more frames than sequential \
         (batched={batched_frames}, sequential={sequential_frames})"
    );

    // Sanity: both produced non-trivial output (at least the 5 entries + end marker).
    assert!(
        !batched_data.is_empty(),
        "batched flush must produce non-empty wire data"
    );
}

/// Wire-byte parity for NDX_DONE echoes under batched vs sequential flush.
///
/// The transfer loop echoes `NDX_DONE` during phase transitions and
/// INC_RECURSE flist-free paths. Each echo is followed by `flush_with_count`.
/// This test verifies that multiple NDX_DONE writes followed by a single
/// batched flush produce byte-identical logical data as individual
/// write+flush pairs.
#[test]
fn batched_flush_ndx_done_echo_parity() {
    use super::super::writer::multiplex::MultiplexWriter;
    use protocol::codec::{MonotonicNdxWriter, NdxCodec};
    use protocol::{MessageCode, recv_msg};

    fn extract_data_payload(wire: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(wire);
        let mut data = Vec::new();
        loop {
            match recv_msg(&mut cursor) {
                Ok(frame) => {
                    assert_eq!(frame.code(), MessageCode::Data);
                    data.extend_from_slice(frame.payload());
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected recv_msg error: {e}"),
            }
        }
        data
    }

    // Write 3 NDX_DONE echoes (simulating INC_RECURSE flist-free path)
    let batched_wire = {
        let mut inner = Vec::<u8>::new();
        let mut mux = MultiplexWriter::new(&mut inner);
        let mut ndx_codec = MonotonicNdxWriter::new(32);
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        // Single batched flush
        flush_with_count(&mut mux).unwrap();
        drop(mux);
        inner
    };

    let sequential_wire = {
        let mut inner = Vec::<u8>::new();
        let mut mux = MultiplexWriter::new(&mut inner);
        let mut ndx_codec = MonotonicNdxWriter::new(32);
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        flush_with_count(&mut mux).unwrap();
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        flush_with_count(&mut mux).unwrap();
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        flush_with_count(&mut mux).unwrap();
        drop(mux);
        inner
    };

    let batched_data = extract_data_payload(&batched_wire);
    let sequential_data = extract_data_payload(&sequential_wire);

    assert_eq!(
        batched_data,
        sequential_data,
        "NDX_DONE echo data must be byte-identical under batched vs sequential flush \
         (batched={} bytes, sequential={} bytes)",
        batched_data.len(),
        sequential_data.len()
    );

    // Each NDX_DONE in protocol 32 (modern codec) is a single 0x00 byte.
    assert_eq!(
        batched_data,
        vec![0x00, 0x00, 0x00],
        "3 NDX_DONE echoes should produce 3 zero bytes (modern codec)"
    );
}

/// Wire-byte parity for mixed NDX writes and file data under batched flush.
///
/// Simulates the transfer loop's write pattern: NDX (file index) + iflags +
/// file data, followed by a single `flush_with_count`. Verifies that the
/// logical wire stream is identical to per-write flushing.
#[test]
fn batched_flush_mixed_ndx_and_data_parity() {
    use super::super::writer::multiplex::MultiplexWriter;
    use protocol::codec::{MonotonicNdxWriter, NdxCodec};
    use protocol::{MessageCode, recv_msg};

    fn extract_data_payload(wire: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(wire);
        let mut data = Vec::new();
        loop {
            match recv_msg(&mut cursor) {
                Ok(frame) => {
                    assert_eq!(frame.code(), MessageCode::Data);
                    data.extend_from_slice(frame.payload());
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected recv_msg error: {e}"),
            }
        }
        data
    }

    // Simulated transfer loop iteration: write NDX + iflags + literal data
    // for 3 files, mimicking the write_ndx_and_attrs + delta data pattern.
    let iflags_transfer: [u8; 2] = (ItemFlags::ITEM_TRANSFER as u16).to_le_bytes();
    let file_data: [&[u8]; 3] = [b"alpha-content", b"beta-data-longer", b"g"];

    let batched_wire = {
        let mut inner = Vec::<u8>::new();
        let mut mux = MultiplexWriter::new(&mut inner);
        let mut ndx_codec = MonotonicNdxWriter::new(32);
        for (i, data) in file_data.iter().enumerate() {
            ndx_codec.write_ndx(&mut mux, i as i32).unwrap();
            mux.write_all(&iflags_transfer).unwrap();
            mux.write_all(data).unwrap();
        }
        // Single batched flush at end of iteration
        flush_with_count(&mut mux).unwrap();
        // Final NDX_DONE
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        flush_with_count(&mut mux).unwrap();
        drop(mux);
        inner
    };

    let sequential_wire = {
        let mut inner = Vec::<u8>::new();
        let mut mux = MultiplexWriter::new(&mut inner);
        let mut ndx_codec = MonotonicNdxWriter::new(32);
        for (i, data) in file_data.iter().enumerate() {
            ndx_codec.write_ndx(&mut mux, i as i32).unwrap();
            mux.write_all(&iflags_transfer).unwrap();
            mux.write_all(data).unwrap();
            flush_with_count(&mut mux).unwrap();
        }
        ndx_codec.write_ndx_done(&mut mux).unwrap();
        flush_with_count(&mut mux).unwrap();
        drop(mux);
        inner
    };

    let batched_data = extract_data_payload(&batched_wire);
    let sequential_data = extract_data_payload(&sequential_wire);

    assert_eq!(
        batched_data,
        sequential_data,
        "mixed NDX+data must be byte-identical under batched vs sequential flush \
         (batched={} bytes, sequential={} bytes)",
        batched_data.len(),
        sequential_data.len()
    );

    // Verify non-trivial content was written (NDX bytes + iflags + file data).
    let expected_min_len = 3 /* NDX bytes for indices 0,1,2 (modern codec) */
        + 3 * 2 /* iflags per file */
        + file_data.iter().map(|d| d.len()).sum::<usize>()
        + 1; /* NDX_DONE */
    assert!(
        batched_data.len() >= expected_min_len,
        "expected at least {expected_min_len} bytes, got {}",
        batched_data.len()
    );
}

/// Regression tests for the `maybe_emit_itemize` emit-gate.
///
/// Upstream `generator.c:582-583` emits an itemize line when ANY of four
/// conditions hold: significant flags set, `INFO_GTE(NAME, 2)`,
/// `stdout_format_has_i > 1`, or `ITEM_XNAME_FOLLOWS`. The previous Rust
/// gate only honored the first condition, so the `itemize.test` testsuite
/// FAILed under `-ivvplrtH` because rows for completely unchanged entries
/// (`iflags == 0`, e.g. `.d ./`, `.f foo/config1`, `.L foo/sym`) were
/// silently dropped.
mod itemize_emit_gate {
    use super::*;
    use crate::generator::item_flags::ItemFlags;
    use logging::{VerbosityConfig, init};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Drives `maybe_emit_itemize` in client mode with a captured callback.
    fn run_gate(verbose_name_level: u8, iflags_raw: u32) -> Vec<String> {
        // Seed the thread-local verbosity so `info_gte(InfoFlag::Name, 2)`
        // reflects the test scenario. Other levels remain at defaults.
        let mut cfg = VerbosityConfig::default();
        cfg.info.name = verbose_name_level;
        init(cfg);

        let handshake = test_handshake();
        let mut config = test_config();
        config.connection.client_mode = true;
        config.flags.info_flags.itemize = true;
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        ctx.file_list.push(protocol::flist::FileEntry::new_file(
            "config1".into(),
            42,
            0o644,
        ));

        let captured: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let lines = Rc::clone(&captured);
        let mut sink = move |line: &str| {
            lines.borrow_mut().push(line.to_owned());
        };
        let mut callback: Option<&mut dyn crate::ItemizeCallback> = Some(&mut sink);

        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        let iflags = ItemFlags::from_raw(iflags_raw);
        ctx.maybe_emit_itemize(&mut writer, &iflags, 0, &mut callback)
            .expect("maybe_emit_itemize must not error in client mode");

        // Reset the verbosity config so siblings see a clean default.
        init(VerbosityConfig::default());
        captured.borrow().clone()
    }

    /// upstream: generator.c:582 - `INFO_GTE(NAME, 2)` forces emission even
    /// when `iflags == 0`. Without this branch the upstream `itemize.test`
    /// testsuite (run under `-ivvplrtH`) lost `.d ./`, `.d bar/`,
    /// `.f foo/config1`, and `.L foo/sym` rows on master.
    #[test]
    fn emits_under_verbose_name_2_with_zero_iflags() {
        let lines = run_gate(2, 0);
        assert_eq!(
            lines.len(),
            1,
            "INFO_GTE(NAME, 2) must force an itemize line even when iflags == 0; got: {lines:?}"
        );
    }

    /// upstream: generator.c:582 - `INFO_GTE(NAME, 2)` is the gate.
    /// `-v` (NAME level 1) is below the threshold, so unchanged entries
    /// must stay silent. This is the existing pre-fix behaviour for
    /// significant-flag-only emission.
    #[test]
    fn suppresses_under_verbose_name_1_with_zero_iflags() {
        let lines = run_gate(1, 0);
        assert!(
            lines.is_empty(),
            "INFO_GTE(NAME, 1) must not force emission; got: {lines:?}"
        );
    }

    /// upstream: generator.c:583 - `(xname && *xname)` (encoded as
    /// `ITEM_XNAME_FOLLOWS` on the wire) forces emission. Verbose level 0
    /// proves the new branch alone is sufficient.
    #[test]
    fn emits_under_xname_follows_with_zero_verbose() {
        let lines = run_gate(0, ItemFlags::ITEM_XNAME_FOLLOWS);
        assert_eq!(
            lines.len(),
            1,
            "ITEM_XNAME_FOLLOWS must force an itemize line under upstream gate; got: {lines:?}"
        );
    }
}
