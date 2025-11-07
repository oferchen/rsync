mod error_helper_tests {
    use super::error::{
        MAX_DELETE_EXIT_CODE, PROTOCOL_INCOMPATIBLE_EXIT_CODE, compile_filter_error,
        daemon_access_denied_error, daemon_authentication_failed_error,
        daemon_authentication_required_error, daemon_error, daemon_protocol_error,
        invalid_argument_error, io_error, map_local_copy_error, missing_operands_error,
        socket_error,
    };
    use super::*;
    use oc_rsync_engine::local_copy::{LocalCopyArgumentError, LocalCopyError};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn render(error: &ClientError) -> String {
        error.message().to_string()
    }

    #[test]
    fn missing_operands_error_includes_hint_and_exit_code() {
        let error = missing_operands_error();

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let rendered = render(&error);
        assert!(rendered.contains("missing source operands"), "{rendered}");
        assert!(rendered.contains("[client="), "{rendered}");
    }

    #[test]
    fn invalid_argument_error_preserves_exit_code_and_message() {
        let error = invalid_argument_error("invalid size", 42);

        assert_eq!(error.exit_code(), 42);
        let rendered = render(&error);
        assert!(rendered.contains("invalid size"), "{rendered}");
    }

    #[test]
    fn compile_filter_error_reports_pattern_and_error() {
        let error = compile_filter_error("*.tmp", &"syntax error");
        let rendered = render(&error);

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(rendered.contains("failed to compile filter pattern '*.tmp'"), "{rendered}");
        assert!(rendered.contains("syntax error"), "{rendered}");
    }

    #[test]
    fn map_local_copy_error_for_missing_operands_mirrors_helper() {
        let mapped = map_local_copy_error(LocalCopyError::missing_operands());
        let rendered = render(&mapped);

        assert_eq!(mapped.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(rendered.contains("missing source operands"), "{rendered}");
    }

    #[test]
    fn map_local_copy_error_for_invalid_argument_preserves_reason() {
        let source =
            LocalCopyError::invalid_argument(LocalCopyArgumentError::EmptyDestinationOperand);
        let exit_code = source.exit_code();
        let mapped = map_local_copy_error(source);
        let rendered = render(&mapped);

        assert_eq!(mapped.exit_code(), exit_code);
        assert!(
            rendered.contains("destination operand must be non-empty"),
            "{rendered}"
        );
    }

    #[test]
    fn map_local_copy_error_for_io_variant_includes_context() {
        let source = LocalCopyError::io(
            "copy file",
            PathBuf::from("dest"),
            io::Error::other("disk full"),
        );
        let mapped = map_local_copy_error(source);
        let rendered = render(&mapped);

        assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(rendered.contains("copy file"), "{rendered}");
        assert!(rendered.contains("dest"), "{rendered}");
        assert!(rendered.contains("disk full"), "{rendered}");
    }

    #[test]
    fn map_local_copy_error_for_timeout_formats_duration() {
        let source = LocalCopyError::timeout(Duration::from_millis(1500));
        let mapped = map_local_copy_error(source);
        let rendered = render(&mapped);

        assert!(rendered.contains("1.500"), "{rendered}");
    }

    #[test]
    fn map_local_copy_error_for_stop_at_reports_message() {
        let deadline = std::time::SystemTime::now();
        let exit_code = LocalCopyError::stop_at_reached(deadline).exit_code();
        let mapped = map_local_copy_error(LocalCopyError::stop_at_reached(deadline));
        let rendered = render(&mapped);

        assert_eq!(mapped.exit_code(), exit_code);
        assert!(rendered.contains("stopping at requested limit"), "{rendered}");
    }

    #[test]
    fn map_local_copy_error_for_delete_limit_handles_pluralisation() {
        let singular = map_local_copy_error(LocalCopyError::delete_limit_exceeded(1));
        let plural = map_local_copy_error(LocalCopyError::delete_limit_exceeded(2));

        assert_eq!(singular.exit_code(), MAX_DELETE_EXIT_CODE);
        assert_eq!(plural.exit_code(), MAX_DELETE_EXIT_CODE);

        let singular_rendered = render(&singular);
        let plural_rendered = render(&plural);

        assert!(singular_rendered.contains("1 entry skipped"), "{singular_rendered}");
        assert!(plural_rendered.contains("2 entries skipped"), "{plural_rendered}");
    }

    #[test]
    fn io_error_carries_action_and_path_context() {
        let error = io_error(
            "write file",
            Path::new("/tmp/data"),
            io::Error::other("access denied"),
        );
        let rendered = render(&error);

        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(rendered.contains("write file"), "{rendered}");
        assert!(rendered.contains("/tmp/data"), "{rendered}");
        assert!(rendered.contains("access denied"), "{rendered}");
    }

    #[test]
    fn socket_error_captures_target() {
        let error = socket_error(
            "connect to",
            "rsync://example.com",
            io::Error::new(io::ErrorKind::TimedOut, "timed out"),
        );
        let rendered = render(&error);

        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        assert!(rendered.contains("connect to rsync://example.com"), "{rendered}");
        assert!(rendered.contains("timed out"), "{rendered}");
    }

    #[test]
    fn daemon_error_forwards_text_and_exit_code() {
        let error = daemon_error("unexpected banner", 15);
        let rendered = render(&error);

        assert_eq!(error.exit_code(), 15);
        assert!(rendered.contains("unexpected banner"), "{rendered}");
    }

    #[test]
    fn daemon_protocol_error_wraps_message() {
        let error = daemon_protocol_error("garbled response");
        let rendered = render(&error);

        assert_eq!(error.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
        assert!(
            rendered.contains("unexpected response from daemon: garbled response"),
            "{rendered}"
        );
    }

    #[test]
    fn daemon_authentication_required_error_formats_reason() {
        let without_reason = daemon_authentication_required_error("");
        let with_reason = daemon_authentication_required_error("token expired");

        let without_rendered = render(&without_reason);
        let with_rendered = render(&with_reason);

        assert!(
            without_rendered.contains("daemon requires authentication for module listing"),
            "{without_rendered}"
        );
        assert!(with_rendered.contains(": token expired"), "{with_rendered}");
    }

    #[test]
    fn daemon_authentication_failed_error_formats_optional_reason() {
        let without_reason = daemon_authentication_failed_error(None);
        let with_empty_reason = daemon_authentication_failed_error(Some(""));
        let with_reason = daemon_authentication_failed_error(Some("bad password"));

        let without_rendered = render(&without_reason);
        let empty_rendered = render(&with_empty_reason);
        let with_rendered = render(&with_reason);

        assert!(
            without_rendered.contains("daemon rejected provided credentials"),
            "{without_rendered}"
        );
        assert!(empty_rendered.contains("daemon rejected provided credentials"));
        assert!(with_rendered.contains(": bad password"), "{with_rendered}");
    }

    #[test]
    fn daemon_access_denied_error_formats_optional_reason() {
        let without_reason = daemon_access_denied_error("");
        let with_reason = daemon_access_denied_error("ip filtered");

        let without_rendered = render(&without_reason);
        let with_rendered = render(&with_reason);

        assert!(
            without_rendered.contains("daemon denied access to module listing"),
            "{without_rendered}"
        );
        assert!(with_rendered.contains(": ip filtered"), "{with_rendered}");
    }
}
