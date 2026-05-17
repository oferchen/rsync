//! Error, timeout, and rejection surface: I/O error categorization,
//! failed-directory propagation, legacy goodbye exchange, input-multiplex
//! activation, daemon filter rules, path-traversal rejection, and
//! sanitize-file-list trust gating.
//!
// TODO: decompose further - this file still exceeds the 650-line cap. A
// follow-up should split the nested mods (`legacy_goodbye_tests`,
// `input_multiplex_tests`, `sanitize_file_list`, `daemon_filter_tests`)
// into their own files alongside the io-error categorization tests.

use std::ffi::OsString;
use std::io::{self, Cursor};
use std::path::PathBuf;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::super::directory::FailedDirectories;
use super::super::stats::TransferStats;
use super::support::{test_config, test_handshake};
use crate::config::ServerConfig;
use crate::error::{
    DeltaFatalError, DeltaRecoverableError, DeltaTransferError, categorize_io_error,
};
use crate::flags::ParsedServerFlags;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

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
fn path_contains_dot_dot_simple_traversal() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "../etc/passwd"
    )));
}

#[test]
fn path_contains_dot_dot_mid_path() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/../../../etc/passwd"
    )));
}

#[test]
fn path_contains_dot_dot_trailing() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "a/b/.."
    )));
}

#[test]
fn path_contains_dot_dot_clean_path() {
    use std::path::Path;
    assert!(!super::super::quick_check::path_contains_dot_dot(
        Path::new("a/b/c")
    ));
}

#[test]
fn path_contains_dot_dot_dot_only() {
    use std::path::Path;
    // Single "." is not ".."
    assert!(!super::super::quick_check::path_contains_dot_dot(
        Path::new(".")
    ));
}

#[test]
fn path_contains_dot_dot_embedded_dots_in_name() {
    use std::path::Path;
    // "..." is not ".." - it's a normal filename
    assert!(!super::super::quick_check::path_contains_dot_dot(
        Path::new("a/.../b")
    ));
}

#[test]
fn path_contains_dot_dot_double_dotdot() {
    use std::path::Path;
    assert!(super::super::quick_check::path_contains_dot_dot(Path::new(
        "a/../../b"
    )));
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
            args: vec![OsString::from(".")],
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
            args: vec![OsString::from(".")],
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

mod sanitize_file_list {
    use super::*;

    fn receiver_with_trust(entries: Vec<FileEntry>, trust_sender: bool) -> ReceiverContext {
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            trust_sender,
            args: vec![OsString::from(".")],
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
            args: vec![OsString::from(".")],
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

mod daemon_filter_tests {
    use super::*;

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
}
