use super::super::connection::DaemonTransferRequest;
use super::arguments::build_full_daemon_args;
use super::server_config::{build_server_config_for_generator, build_server_config_for_receiver};
use super::transfer::{is_dry_run_remote_close, read_files_from_for_forwarding};

use crate::client::config::ClientConfig;
use crate::client::module_list::DaemonAddress;

use protocol::ProtocolVersion;

mod protect_args_daemon_tests {
    use super::super::arguments::build_minimal_daemon_args;
    use super::*;

    fn test_daemon_request() -> DaemonTransferRequest {
        DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
            module: "test".to_owned(),
            path: String::new(),
            username: None,
        }
    }

    #[test]
    fn build_minimal_args_receiver() {
        let args = build_minimal_daemon_args(false);
        assert_eq!(args, vec!["--server", "-s", "."]);
    }

    #[test]
    fn build_minimal_args_sender() {
        let args = build_minimal_daemon_args(true);
        assert_eq!(args, vec!["--server", "--sender", "-s", "."]);
    }

    #[test]
    fn build_full_args_includes_module_path() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert_eq!(args[0], "--server");
        assert!(args.contains(&".".to_owned()));
        let last = args.last().unwrap();
        assert!(last.starts_with(&request.module));
    }

    #[test]
    fn build_full_args_sender_flag() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert_eq!(args[0], "--server");
        assert_eq!(args[1], "--sender");
    }

    #[test]
    fn build_full_args_capability_flags_protocol30() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(args.iter().any(|a| a.starts_with("-e.")));
    }

    #[test]
    fn build_full_args_no_capability_flags_protocol29() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(29u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(!args.iter().any(|a| a.starts_with("-e.")));
    }

    #[test]
    fn build_full_args_push_advertises_inc_recurse_capability_by_default() {
        // Push (is_sender=false, we are sender) advertises the 'i' capability
        // by default, mirroring upstream's `allow_inc_recurse = 1`. Tracker #1862.
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        let caps = args
            .iter()
            .find(|a| a.starts_with("-e."))
            .expect("capability string present");
        assert!(
            caps.contains('i'),
            "default push capability string must advertise 'i': {caps}"
        );
    }

    #[test]
    fn build_full_args_push_omits_inc_recurse_when_no_inc_recursive_set() {
        // `--no-inc-recursive` clears `allow_inc_recurse`; the bit is dropped
        // from the capability string in both transfer directions. Tracker #1862.
        let config = ClientConfig::builder().inc_recursive_send(false).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        let caps = args
            .iter()
            .find(|a| a.starts_with("-e."))
            .expect("capability string present");
        assert!(
            !caps.contains('i'),
            "--no-inc-recursive must suppress 'i' on push capability: {caps}"
        );
    }

    #[test]
    fn build_full_args_pull_advertises_inc_recurse_capability_by_default() {
        // Pull (is_sender=true, daemon is sender) also advertises 'i' by
        // default to match upstream's symmetric `allow_inc_recurse` global.
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let request = test_daemon_request();

        let config_default = ClientConfig::default();
        let args_default = build_full_daemon_args(&config_default, &request, protocol, true);
        let caps_default = args_default
            .iter()
            .find(|a| a.starts_with("-e."))
            .expect("capability string present");
        assert!(caps_default.contains('i'));

        let config_off = ClientConfig::builder().inc_recursive_send(false).build();
        let args_off = build_full_daemon_args(&config_off, &request, protocol, true);
        let caps_off = args_off
            .iter()
            .find(|a| a.starts_with("-e."))
            .expect("capability string present");
        assert!(
            !caps_off.contains('i'),
            "--no-inc-recursive must suppress 'i' on pull capability: {caps_off}"
        );
    }

    #[test]
    fn build_full_args_includes_compare_dest() {
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--compare-dest=/tmp/compare"),
            "expected --compare-dest=/tmp/compare in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_copy_dest() {
        let config = ClientConfig::builder()
            .copy_destination("/tmp/copy")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--copy-dest=/tmp/copy"),
            "expected --copy-dest=/tmp/copy in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_link_dest() {
        let config = ClientConfig::builder().link_destination("/prev").build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--link-dest=/prev"),
            "expected --link-dest=/prev in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_multiple_reference_dirs() {
        let config = ClientConfig::builder()
            .link_destination("/prev1")
            .link_destination("/prev2")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--link-dest=/prev1"),
            "expected --link-dest=/prev1 in args: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--link-dest=/prev2"),
            "expected --link-dest=/prev2 in args: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_reference_dirs_when_empty() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--compare-dest=")
                || a.starts_with("--copy-dest=")
                || a.starts_with("--link-dest=")),
            "should not emit reference dir args when empty: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_reference_dirs_in_pull_mode() {
        // upstream: options.c:2915-2923 - reference dirs are inside if(am_sender).
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .link_destination("/prev")
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            !args.iter().any(|a| a.starts_with("--compare-dest=")
                || a.starts_with("--copy-dest=")
                || a.starts_with("--link-dest=")),
            "pull mode should not send reference dir args to daemon: {args:?}"
        );
    }

    #[test]
    fn build_full_args_includes_log_format_for_itemize_push() {
        // upstream: options.c:2750-2762 - --log-format=%i sent when am_sender
        // (client is sender / push mode) so daemon receiver emits itemize via MSG_INFO.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // is_sender=false means daemon is NOT sender, i.e., client IS sender (push)
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--log-format=%i"),
            "push with itemize should include --log-format=%i: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_log_format_for_itemize_pull() {
        // upstream: options.c:2752 - --log-format only sent when am_sender.
        // In pull mode (daemon is sender), client handles itemize locally.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        // is_sender=true means daemon IS sender (pull)
        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            !args.iter().any(|a| a == "--log-format=%i"),
            "pull with itemize should not include --log-format=%i: {args:?}"
        );
    }

    #[test]
    fn build_full_args_omits_log_format_without_itemize() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--log-format")),
            "should not include --log-format without itemize: {args:?}"
        );
    }
}

mod server_config_reference_dirs {
    use super::*;
    use crate::client::config::ReferenceDirectoryKind;

    #[test]
    fn receiver_config_propagates_reference_directories() {
        let config = ClientConfig::builder()
            .compare_destination("/tmp/compare")
            .link_destination("/prev")
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();

        assert_eq!(server_config.reference_directories.len(), 2);
        assert_eq!(
            server_config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(
            server_config.reference_directories[0]
                .path()
                .to_str()
                .unwrap(),
            "/tmp/compare"
        );
        assert_eq!(
            server_config.reference_directories[1].kind(),
            ReferenceDirectoryKind::Link
        );
        assert_eq!(
            server_config.reference_directories[1]
                .path()
                .to_str()
                .unwrap(),
            "/prev"
        );
    }

    #[test]
    fn generator_config_propagates_reference_directories() {
        let config = ClientConfig::builder()
            .copy_destination("/tmp/copy")
            .build();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert_eq!(server_config.reference_directories.len(), 1);
        assert_eq!(
            server_config.reference_directories[0].kind(),
            ReferenceDirectoryKind::Copy
        );
        assert_eq!(
            server_config.reference_directories[0]
                .path()
                .to_str()
                .unwrap(),
            "/tmp/copy"
        );
    }

    #[test]
    fn receiver_config_empty_reference_dirs_by_default() {
        let config = ClientConfig::default();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();

        assert!(server_config.reference_directories.is_empty());
    }

    #[test]
    fn generator_config_empty_reference_dirs_by_default() {
        let config = ClientConfig::default();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert!(server_config.reference_directories.is_empty());
    }

    #[test]
    fn generator_config_sets_files_from_for_local_file_push() {
        // upstream: options.c:2944 - when the client is the sender and
        // --files-from points to a local file, the generator reads filenames
        // directly from the file (not via the protocol stream).
        let config = ClientConfig::builder()
            .files_from(crate::client::config::FilesFromSource::LocalFile(
                std::path::PathBuf::from("/tmp/list.txt"),
            ))
            .build();

        let local_paths = vec!["src/".to_owned()];
        let server_config =
            build_server_config_for_generator(&config, &local_paths, Vec::new()).unwrap();

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("/tmp/list.txt"),
            "generator should read files-from from local file for push"
        );
    }

    #[test]
    fn generator_config_sets_files_from_for_remote_source() {
        let config = ClientConfig::builder()
            .files_from(crate::client::config::FilesFromSource::RemoteFile(
                "/srv/list.txt".to_owned(),
            ))
            .build();

        let local_paths = vec!["src/".to_owned()];
        let server_config =
            build_server_config_for_generator(&config, &local_paths, Vec::new()).unwrap();

        assert_eq!(
            server_config.file_selection.files_from_path.as_deref(),
            Some("-"),
            "generator should read files-from from protocol stream for remote source"
        );
        assert!(
            server_config.file_selection.from0,
            "remote files-from uses NUL-separated wire format"
        );
    }

    #[test]
    fn generator_config_propagates_itemize_flag() {
        // upstream: options.c:2750-2762 - the local ServerConfig must have
        // info_flags.itemize set so the generator's maybe_emit_itemize()
        // produces client-side output via the callback.
        let config = ClientConfig::builder().itemize_changes(true).build();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert!(
            server_config.flags.info_flags.itemize,
            "itemize_changes should propagate to server config info_flags"
        );
    }

    #[test]
    fn generator_config_itemize_default_false() {
        let config = ClientConfig::default();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();

        assert!(
            !server_config.flags.info_flags.itemize,
            "itemize should be false by default"
        );
    }
}

mod dry_run_remote_close_tests {
    use super::*;
    use std::io;

    #[test]
    fn broken_pipe_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn connection_reset_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::ConnectionReset, "connection reset");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn connection_aborted_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::ConnectionAborted, "connection aborted");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn unexpected_eof_is_remote_close() {
        let err = io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected eof");
        assert!(is_dry_run_remote_close(&err));
    }

    #[test]
    fn timeout_is_not_remote_close() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "timed out");
        assert!(!is_dry_run_remote_close(&err));
    }

    #[test]
    fn permission_denied_is_not_remote_close() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "permission denied");
        assert!(!is_dry_run_remote_close(&err));
    }

    #[test]
    fn connection_refused_is_not_remote_close() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
        assert!(!is_dry_run_remote_close(&err));
    }

    #[test]
    fn other_error_is_not_remote_close() {
        let err = io::Error::other("some other error");
        assert!(!is_dry_run_remote_close(&err));
    }
}

mod files_from_daemon_args_tests {
    use super::*;
    use crate::client::config::FilesFromSource;
    use std::path::PathBuf;

    fn test_daemon_request() -> DaemonTransferRequest {
        DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
            module: "test".to_owned(),
            path: String::new(),
            username: None,
        }
    }

    #[test]
    fn push_with_local_file_omits_files_from_arg() {
        // upstream: options.c:2944 - when client is sender and files_from
        // is local, the arg is NOT sent to the daemon.
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--files-from")),
            "push should not send --files-from to daemon: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--from0"),
            "push should not send --from0 to daemon: {args:?}"
        );
    }

    #[test]
    fn push_with_stdin_omits_files_from_arg() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::Stdin)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--files-from")),
            "push with stdin should not send --files-from to daemon: {args:?}"
        );
    }

    #[test]
    fn pull_with_local_file_sends_files_from_stdin() {
        // upstream: options.c:2944 - when client is receiver (pull), local
        // files are forwarded as --files-from=- with --from0.
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::LocalFile(PathBuf::from("/tmp/list.txt")))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--files-from=-"),
            "pull should send --files-from=- to daemon: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--from0"),
            "pull should send --from0 to daemon: {args:?}"
        );
    }

    #[test]
    fn pull_with_stdin_sends_files_from_stdin() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::Stdin)
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--files-from=-"),
            "pull with stdin should send --files-from=- to daemon: {args:?}"
        );
    }

    #[test]
    fn push_with_remote_file_sends_files_from_path() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            args.iter().any(|a| a == "--files-from=/remote/list.txt"),
            "should send remote --files-from path: {args:?}"
        );
    }

    #[test]
    fn pull_with_remote_file_sends_files_from_path() {
        let config = ClientConfig::builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, true);

        assert!(
            args.iter().any(|a| a == "--files-from=/remote/list.txt"),
            "should send remote --files-from path: {args:?}"
        );
    }

    #[test]
    fn no_files_from_omits_arg() {
        let config = ClientConfig::default();
        let request = test_daemon_request();
        let protocol = ProtocolVersion::try_from(32u8).unwrap();

        let args = build_full_daemon_args(&config, &request, protocol, false);

        assert!(
            !args.iter().any(|a| a.starts_with("--files-from")),
            "should not include --files-from: {args:?}"
        );
    }
}

mod files_from_forwarding_tests {
    use super::*;
    use crate::client::config::{ClientConfigBuilder, FilesFromSource};
    use std::io::Cursor;

    fn test_builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default().transfer_args(["/src", "rsync://host/mod/"])
    }

    #[test]
    fn read_from_local_file_newline_delimited() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "file1.txt\nfile2.txt\nsubdir/file3.txt\n").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert_eq!(
            filenames,
            vec!["file1.txt", "file2.txt", "subdir/file3.txt"]
        );
    }

    #[test]
    fn read_from_local_file_nul_delimited() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "alpha.txt\0beta.txt\0").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .from0(true)
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert_eq!(filenames, vec!["alpha.txt", "beta.txt"]);
    }

    #[test]
    fn read_from_nonexistent_file_returns_error() {
        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(std::path::PathBuf::from(
                "/nonexistent/list.txt",
            )))
            .build();

        let err = read_files_from_for_forwarding(&config).unwrap_err();
        assert!(err.to_string().contains("failed to open --files-from"));
    }

    #[test]
    fn no_forwarding_for_none() {
        let config = test_builder().build();

        let data = read_files_from_for_forwarding(&config).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn no_forwarding_for_remote_file() {
        let config = test_builder()
            .files_from(FilesFromSource::RemoteFile("/remote/list.txt".to_owned()))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn empty_local_file_produces_terminator() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("empty.txt");
        std::fs::write(&list_file, "").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        assert_eq!(data, b"\0\0");

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert!(filenames.is_empty());
    }

    #[test]
    fn roundtrip_with_crlf_line_endings() {
        let dir = test_support::create_tempdir();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "file1.txt\r\nfile2.txt\r\n").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader, None).unwrap();
        assert_eq!(filenames, vec!["file1.txt", "file2.txt"]);
    }

    #[test]
    fn files_from_data_on_connection_config() {
        use transfer::config::ConnectionConfig;

        let mut conn = ConnectionConfig::default();
        assert!(conn.files_from_data.is_none());

        conn.files_from_data = Some(b"file1.txt\0file2.txt\0\0".to_vec());
        assert!(conn.files_from_data.is_some());

        let data = conn.files_from_data.take().unwrap();
        assert_eq!(data, b"file1.txt\0file2.txt\0\0");
        assert!(conn.files_from_data.is_none());
    }
}

mod iconv_bridge {
    //! Integration tests for the `--iconv` bridge from `IconvSetting`
    //! to `ConnectionConfig.iconv` (closes #1911).

    use super::*;
    use crate::client::config::IconvSetting;

    #[test]
    fn unspecified_setting_leaves_connection_iconv_none() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Unspecified)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        assert!(server_config.connection.iconv.is_none());
    }

    #[test]
    fn disabled_setting_leaves_connection_iconv_none() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Disabled)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        assert!(server_config.connection.iconv.is_none());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn explicit_setting_populates_connection_iconv_for_receiver() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: Some("ISO-8859-1".to_owned()),
            })
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        let converter = server_config
            .connection
            .iconv
            .expect("--iconv=utf-8,latin1 must produce a converter on the receiver path");
        // upstream: rsync.c:130-140 - wire is always UTF-8. When the local
        // charset matches the wire charset, this peer's converter is
        // identity (no transcoding on the local->wire direction). The
        // `remote` half of LOCAL,REMOTE is the peer's local charset and is
        // forwarded to the remote CLI separately, not consumed here.
        assert!(converter.is_identity());
        assert_eq!(converter.local_encoding_name(), "UTF-8");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn explicit_setting_populates_connection_iconv_for_generator() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: Some("ISO-8859-1".to_owned()),
            })
            .build();
        let server_config =
            build_server_config_for_generator(&config, &["src".to_owned()], Vec::new()).unwrap();
        assert!(server_config.connection.iconv.is_some());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn locale_default_setting_populates_connection_iconv() {
        let config = ClientConfig::builder()
            .iconv(IconvSetting::LocaleDefault)
            .build();
        let server_config =
            build_server_config_for_receiver(&config, &["dest".to_owned()], Vec::new()).unwrap();
        let converter = server_config
            .connection
            .iconv
            .expect("--iconv=. must produce a locale-derived converter");
        // converter_from_locale uses UTF-8 on both sides for portability,
        // making it an identity converter on most modern systems.
        assert!(converter.is_identity());
    }
}
