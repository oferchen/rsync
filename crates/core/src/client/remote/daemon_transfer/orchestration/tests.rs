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
        let dir = tempfile::tempdir().unwrap();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "file1.txt\nfile2.txt\nsubdir/file3.txt\n").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
        assert_eq!(
            filenames,
            vec!["file1.txt", "file2.txt", "subdir/file3.txt"]
        );
    }

    #[test]
    fn read_from_local_file_nul_delimited() {
        let dir = tempfile::tempdir().unwrap();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "alpha.txt\0beta.txt\0").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .from0(true)
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let list_file = dir.path().join("empty.txt");
        std::fs::write(&list_file, "").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        assert_eq!(data, b"\0\0");

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
        assert!(filenames.is_empty());
    }

    #[test]
    fn roundtrip_with_crlf_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let list_file = dir.path().join("list.txt");
        std::fs::write(&list_file, "file1.txt\r\nfile2.txt\r\n").unwrap();

        let config = test_builder()
            .files_from(FilesFromSource::LocalFile(list_file))
            .build();

        let data = read_files_from_for_forwarding(&config).unwrap();

        let mut reader = Cursor::new(&data);
        let filenames = protocol::read_files_from_stream(&mut reader).unwrap();
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
