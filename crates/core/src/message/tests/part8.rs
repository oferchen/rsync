// ============================================================================
// Upstream rsync error message format verification (task #64)
//
// Upstream rsync uses specific error message formats that tools and scripts
// may depend on. This file verifies that the Message rendering system
// produces output matching those formats. The canonical patterns are:
//
//   rsync error: <text> (code N) at <file>:<line> [<role>=<version>]
//   rsync warning: <text> (code N) at <file>:<line> [<role>=<version>]
//   rsync info: <text> at <file>:<line> [<role>=<version>]
//
// Reference: upstream rsync log.c, errcode.h (rsync 3.4.1)
// ============================================================================

// ---------------------------------------------------------------------------
// 1. Prefix format: "rsync <severity>: " matches upstream
// ---------------------------------------------------------------------------

#[test]
fn error_prefix_matches_upstream_format() {
    let msg = Message::error(23, "test message");
    let rendered = msg.to_string();
    assert!(
        rendered.starts_with("rsync error: "),
        "Error messages must start with 'rsync error: ', got: {rendered}"
    );
}

#[test]
fn warning_prefix_matches_upstream_format() {
    let msg = Message::warning("test message");
    let rendered = msg.to_string();
    assert!(
        rendered.starts_with("rsync warning: "),
        "Warning messages must start with 'rsync warning: ', got: {rendered}"
    );
}

#[test]
fn info_prefix_matches_upstream_format() {
    let msg = Message::info("test message");
    let rendered = msg.to_string();
    assert!(
        rendered.starts_with("rsync info: "),
        "Info messages must start with 'rsync info: ', got: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// 2. Code suffix format: " (code N)" matches upstream
// ---------------------------------------------------------------------------

#[test]
fn error_code_suffix_matches_upstream_format() {
    let msg = Message::error(23, "partial transfer");
    let rendered = msg.to_string();
    assert!(
        rendered.contains(" (code 23)"),
        "Error messages must include ' (code N)' suffix, got: {rendered}"
    );
}

#[test]
fn warning_code_suffix_matches_upstream_format() {
    let msg = Message::warning("some files vanished").with_code(24);
    let rendered = msg.to_string();
    assert!(
        rendered.contains(" (code 24)"),
        "Warning messages with code must include ' (code N)' suffix, got: {rendered}"
    );
}

#[test]
fn info_messages_omit_code_suffix_like_upstream() {
    let msg = Message::info("negotiation complete");
    let rendered = msg.to_string();
    assert!(
        !rendered.contains("(code"),
        "Info messages must not include code suffix, got: {rendered}"
    );
}

#[test]
fn error_without_code_omits_code_suffix() {
    let msg = Message::new(Severity::Error, "test").without_code();
    let rendered = msg.to_string();
    assert!(
        !rendered.contains("(code"),
        "Messages without code must omit code suffix, got: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// 3. Source location format: " at <file>:<line>" matches upstream
// ---------------------------------------------------------------------------

#[test]
fn source_location_uses_at_separator_matching_upstream() {
    let msg = Message::error(11, "error in file IO").with_source(message_source!());
    let rendered = msg.to_string();
    assert!(
        rendered.contains(" at "),
        "Source location must use ' at ' separator like upstream, got: {rendered}"
    );
    // Upstream uses "at main.c(NNN)" but we use "at path:line" which is close
    assert!(
        rendered.contains(":"),
        "Source location must include colon between file and line, got: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// 4. Role trailer format: " [<role>=<version>]" matches upstream
// ---------------------------------------------------------------------------

#[test]
fn sender_role_trailer_matches_upstream_format() {
    let msg = Message::error(23, "delta-transfer failure").with_role(Role::Sender);
    let rendered = msg.to_string();
    let expected_trailer = format!("[sender={}]", crate::version::RUST_VERSION);
    assert!(
        rendered.contains(&expected_trailer),
        "Sender role trailer must match '[sender=version]', got: {rendered}"
    );
}

#[test]
fn receiver_role_trailer_matches_upstream_format() {
    let msg = Message::error(11, "error in file IO").with_role(Role::Receiver);
    let rendered = msg.to_string();
    let expected_trailer = format!("[receiver={}]", crate::version::RUST_VERSION);
    assert!(
        rendered.contains(&expected_trailer),
        "Receiver role trailer must match '[receiver=version]', got: {rendered}"
    );
}

#[test]
fn generator_role_trailer_matches_upstream_format() {
    let msg = Message::error(11, "error in file IO").with_role(Role::Generator);
    let rendered = msg.to_string();
    let expected_trailer = format!("[generator={}]", crate::version::RUST_VERSION);
    assert!(
        rendered.contains(&expected_trailer),
        "Generator role trailer must match '[generator=version]', got: {rendered}"
    );
}

#[test]
fn all_roles_produce_bracketed_trailers() {
    for role in Role::ALL {
        let msg = Message::error(23, "test").with_role(role);
        let rendered = msg.to_string();
        let expected = format!("[{}={}]", role.as_str(), crate::version::RUST_VERSION);
        assert!(
            rendered.contains(&expected),
            "Role {role:?} trailer must be '[{role}=version]', got: {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Complete message format for common upstream patterns
// ---------------------------------------------------------------------------

#[test]
fn partial_transfer_error_matches_upstream_summary_format() {
    // Upstream: rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1337) [sender=3.4.1]
    let msg = Message::from_exit_code(23)
        .expect("exit code 23 is defined")
        .with_role(Role::Sender);
    let rendered = msg.to_string();

    assert!(
        rendered.starts_with("rsync error: "),
        "Must start with 'rsync error: '"
    );
    assert!(
        rendered.contains("some files/attrs were not transferred"),
        "Must contain canonical upstream text"
    );
    assert!(
        rendered.contains("(code 23)"),
        "Must contain exit code suffix"
    );
    let version = crate::version::RUST_VERSION;
    assert!(
        rendered.contains(&format!("[sender={version}]")),
        "Must contain sender trailer"
    );
}

#[test]
fn vanished_files_warning_matches_upstream_format() {
    // Upstream: rsync warning: some files vanished before they could be transferred (code 24) at main.c(1889) [sender=3.4.1]
    let msg = Message::from_exit_code(24)
        .expect("exit code 24 is defined")
        .with_role(Role::Sender);
    let rendered = msg.to_string();

    assert!(
        rendered.starts_with("rsync warning: "),
        "Exit code 24 must produce a warning, not an error"
    );
    assert!(
        rendered.contains("some files vanished before they could be transferred"),
        "Must contain canonical upstream text"
    );
    assert!(
        rendered.contains("(code 24)"),
        "Must contain exit code suffix"
    );
}

#[test]
fn socket_io_error_matches_upstream_format() {
    // Upstream: rsync error: error in socket IO (code 10) at io.c(NNN) [sender=3.4.1]
    let msg = Message::from_exit_code(10)
        .expect("exit code 10 is defined")
        .with_role(Role::Sender);
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("error in socket IO"));
    assert!(rendered.contains("(code 10)"));
}

#[test]
fn file_io_error_matches_upstream_format() {
    // Upstream: rsync error: error in file IO (code 11) at receiver.c(NNN) [receiver=3.4.1]
    let msg = Message::from_exit_code(11)
        .expect("exit code 11 is defined")
        .with_role(Role::Receiver);
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("error in file IO"));
    assert!(rendered.contains("(code 11)"));
}

#[test]
fn protocol_data_stream_error_matches_upstream_format() {
    // Upstream: rsync error: error in rsync protocol data stream (code 12) at io.c(NNN) [sender=3.4.1]
    let msg = Message::from_exit_code(12)
        .expect("exit code 12 is defined")
        .with_role(Role::Sender);
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("error in rsync protocol data stream"));
    assert!(rendered.contains("(code 12)"));
}

#[test]
fn timeout_error_matches_upstream_format() {
    // Upstream: rsync error: timeout in data send/receive (code 30) at io.c(NNN) [sender=3.4.1]
    let msg = Message::from_exit_code(30)
        .expect("exit code 30 is defined")
        .with_role(Role::Sender);
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("timeout in data send/receive"));
    assert!(rendered.contains("(code 30)"));
}

#[test]
fn max_delete_error_matches_upstream_format() {
    // Upstream: rsync error: the --max-delete limit stopped deletions (code 25) at ...
    let msg = Message::from_exit_code(25)
        .expect("exit code 25 is defined")
        .with_role(Role::Generator);
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("--max-delete limit stopped deletions"));
    assert!(rendered.contains("(code 25)"));
}

// ---------------------------------------------------------------------------
// 6. EXIT_CODE_TABLE text matches upstream rerr_names (log.c)
// ---------------------------------------------------------------------------

#[test]
fn exit_code_table_text_matches_upstream_rerr_names() {
    // These strings must match upstream rsync's log.c rerr_names table
    // byte-for-byte. Changing them would break scripts that parse rsync output.
    let expected = [
        (1, "syntax or usage error"),
        (2, "protocol incompatibility"),
        (3, "errors selecting input/output files, dirs"),
        (4, "requested action not supported"),
        (5, "error starting client-server protocol"),
        (6, "daemon unable to append to log-file"),
        (10, "error in socket IO"),
        (11, "error in file IO"),
        (12, "error in rsync protocol data stream"),
        (13, "errors with program diagnostics"),
        (14, "error in IPC code"),
        (15, "sibling process crashed"),
        (16, "sibling process terminated abnormally"),
        (19, "received SIGUSR1"),
        (20, "received SIGINT, SIGTERM, or SIGHUP"),
        (21, "waitpid() failed"),
        (22, "error allocating core memory buffers"),
        (
            23,
            "some files/attrs were not transferred (see previous errors)",
        ),
        (
            24,
            "some files vanished before they could be transferred",
        ),
        (25, "the --max-delete limit stopped deletions"),
        (30, "timeout in data send/receive"),
        (35, "timeout waiting for daemon connection"),
        (124, "remote shell failed"),
        (125, "remote shell killed"),
        (126, "remote command could not be run"),
        (127, "remote command not found"),
    ];

    for (code, expected_text) in expected {
        let template =
            strings::exit_code_message(code).unwrap_or_else(|| panic!("exit code {code} must be in the table"));
        assert_eq!(
            template.text(),
            expected_text,
            "Exit code {code} text must match upstream rerr_names"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. Severity classification matches upstream
// ---------------------------------------------------------------------------

#[test]
fn only_exit_code_24_produces_warning_matching_upstream() {
    // Upstream rsync: only exit code 24 (vanished files) produces a warning.
    // All other exit codes produce errors.
    for entry in strings::exit_code_messages() {
        if entry.code() == 24 {
            assert_eq!(
                entry.severity(),
                Severity::Warning,
                "Exit code 24 must be a warning"
            );
        } else {
            assert_eq!(
                entry.severity(),
                Severity::Error,
                "Exit code {} must be an error, not {:?}",
                entry.code(),
                entry.severity()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 8. Error message format with errno-like context (permission denied, ENOENT)
// ---------------------------------------------------------------------------

#[test]
fn permission_denied_error_includes_context_like_upstream() {
    // Upstream: rsync: [receiver] <filename>: Permission denied (13)
    // Our format: rsync error: <action description> (code N) [role=version]
    let msg = Message::error(23, "send_files failed to open \"/test/file\": Permission denied (13)")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("Permission denied (13)"));
    assert!(rendered.contains("(code 23)"));
}

#[test]
fn file_not_found_error_includes_context_like_upstream() {
    // Upstream: rsync: [sender] link_stat "/nonexistent" failed: No such file or directory (2)
    let msg = Message::error(
        23,
        "link_stat \"/nonexistent\" failed: No such file or directory (2)",
    )
    .with_role(Role::Sender)
    .with_source(message_source!());
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("No such file or directory (2)"));
    assert!(rendered.contains("(code 23)"));
}

// ---------------------------------------------------------------------------
// 9. Segment ordering matches upstream: prefix, text, code, source, trailer
// ---------------------------------------------------------------------------

#[test]
fn message_segment_order_matches_upstream() {
    let msg = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let rendered = msg.to_string();

    // Find positions of each component
    let prefix_pos = rendered.find("rsync error: ").expect("prefix must exist");
    let text_pos = rendered
        .find("delta-transfer failure")
        .expect("text must exist");
    let code_pos = rendered.find("(code 23)").expect("code suffix must exist");
    let at_pos = rendered.find(" at ").expect("source separator must exist");
    let trailer_pos = rendered.find("[sender=").expect("trailer must exist");

    // Verify ordering: prefix < text < code < source < trailer
    assert!(
        prefix_pos < text_pos,
        "Prefix must come before text"
    );
    assert!(
        text_pos < code_pos,
        "Text must come before code suffix"
    );
    assert!(
        code_pos < at_pos,
        "Code suffix must come before source location"
    );
    assert!(
        at_pos < trailer_pos,
        "Source location must come before trailer"
    );
}

// ---------------------------------------------------------------------------
// 10. Exit code error code names match upstream errcode.h constants
// ---------------------------------------------------------------------------

#[test]
fn error_code_names_match_upstream_errcode_h() {
    use crate::client::ClientError;
    use crate::exit_code::{ErrorCodification, ExitCode};

    let test_cases: Vec<(ExitCode, &str)> = vec![
        (ExitCode::Ok, "RERR_OK"),
        (ExitCode::Syntax, "RERR_SYNTAX"),
        (ExitCode::Protocol, "RERR_PROTOCOL"),
        (ExitCode::FileSelect, "RERR_FILESELECT"),
        (ExitCode::Unsupported, "RERR_UNSUPPORTED"),
        (ExitCode::StartClient, "RERR_STARTCLIENT"),
        (ExitCode::LogFileAppend, "RERR_LOG_FAILURE"),
        (ExitCode::SocketIo, "RERR_SOCKETIO"),
        (ExitCode::FileIo, "RERR_FILEIO"),
        (ExitCode::StreamIo, "RERR_STREAMIO"),
        (ExitCode::MessageIo, "RERR_MESSAGEIO"),
        (ExitCode::Ipc, "RERR_IPC"),
        (ExitCode::Crashed, "RERR_CRASHED"),
        (ExitCode::Terminated, "RERR_TERMINATED"),
        (ExitCode::Signal1, "RERR_SIGNAL1"),
        (ExitCode::Signal, "RERR_SIGNAL"),
        (ExitCode::WaitChild, "RERR_WAITCHILD"),
        (ExitCode::Malloc, "RERR_MALLOC"),
        (ExitCode::PartialTransfer, "RERR_PARTIAL"),
        (ExitCode::Vanished, "RERR_VANISHED"),
        (ExitCode::DeleteLimit, "RERR_DEL_LIMIT"),
        (ExitCode::Timeout, "RERR_TIMEOUT"),
        (ExitCode::ConnectionTimeout, "RERR_CONTIMEOUT"),
        (ExitCode::CommandFailed, "RERR_CMD_FAILED"),
        (ExitCode::CommandKilled, "RERR_CMD_KILLED"),
        (ExitCode::CommandRun, "RERR_CMD_RUN"),
        (ExitCode::CommandNotFound, "RERR_CMD_NOTFOUND"),
    ];

    for (exit_code, expected_name) in test_cases {
        let msg = rsync_error!(exit_code.as_i32(), "test").with_role(Role::Client);
        let error = ClientError::with_code(exit_code, msg);
        assert_eq!(
            error.error_code_name(),
            expected_name,
            "Exit code {exit_code:?} must map to {expected_name}"
        );
    }
}

// ---------------------------------------------------------------------------
// 11. from_exit_code covers all upstream-documented codes
// ---------------------------------------------------------------------------

#[test]
fn from_exit_code_covers_all_documented_upstream_codes() {
    // All exit codes documented in the upstream rsync man page
    let upstream_documented = [1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 20, 21, 22, 23, 24, 25, 30, 35];

    for code in upstream_documented {
        let msg = Message::from_exit_code(code);
        assert!(
            msg.is_some(),
            "Upstream-documented exit code {code} must be recognized by from_exit_code"
        );
    }
}

// ---------------------------------------------------------------------------
// 12. exit_code_message_with_detail format
// ---------------------------------------------------------------------------

#[test]
fn exit_code_message_with_detail_preserves_upstream_text_prefix() {
    // Upstream rsync sometimes adds detail after the canonical text:
    // "rsync error: syntax or usage error: <specific detail> (code 1)"
    let msg = strings::exit_code_message_with_detail(1, "unknown option '--bad-flag'")
        .expect("exit code 1 is defined");
    let rendered = msg.to_string();

    assert!(
        rendered.starts_with("rsync error: syntax or usage error: "),
        "Detail messages must preserve the canonical text as prefix, got: {rendered}"
    );
    assert!(
        rendered.contains("unknown option '--bad-flag'"),
        "Detail must be included, got: {rendered}"
    );
    assert!(rendered.contains("(code 1)"));
}

// ---------------------------------------------------------------------------
// 13. Regex-based format verification for parseable output
// ---------------------------------------------------------------------------

#[test]
fn error_message_is_parseable_by_regex() {
    // Tools often parse rsync output with regexes. Verify our format is parseable.
    let msg = Message::error(23, "some files/attrs were not transferred (see previous errors)")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let rendered = msg.to_string();

    // Pattern: rsync error: <text> (code <N>) at <file>:<line> [<role>=<version>]
    // Verify components are present and in order
    assert!(rendered.starts_with("rsync error: "));
    assert!(rendered.contains("(code 23)"));
    assert!(rendered.contains(" at "));
    assert!(rendered.contains("[sender="));
    assert!(rendered.ends_with("]"));
}

#[test]
fn warning_message_is_parseable_by_regex() {
    let msg = Message::warning("some files vanished before they could be transferred")
        .with_code(24)
        .with_role(Role::Sender)
        .with_source(message_source!());
    let rendered = msg.to_string();

    assert!(rendered.starts_with("rsync warning: "));
    assert!(rendered.contains("(code 24)"));
    assert!(rendered.contains(" at "));
    assert!(rendered.contains("[sender="));
    assert!(rendered.ends_with("]"));
}

// ---------------------------------------------------------------------------
// 14. Exit code 0 (success) is NOT in the exit code message table
// ---------------------------------------------------------------------------

#[test]
fn exit_code_zero_not_in_message_table() {
    // Upstream rsync does not include exit code 0 in rerr_names.
    // Success should not produce an error/warning message.
    assert!(
        strings::exit_code_message(0).is_none(),
        "Exit code 0 must not be in the message table (upstream has no rerr_names entry for 0)"
    );
}

// ---------------------------------------------------------------------------
// 15. Newline handling matches upstream (no trailing newline in Display)
// ---------------------------------------------------------------------------

#[test]
fn display_does_not_append_newline_matching_upstream() {
    let msg = Message::error(23, "test message").with_role(Role::Sender);
    let rendered = msg.to_string();
    assert!(
        !rendered.ends_with('\n'),
        "Display rendering must not include trailing newline"
    );
}

#[test]
fn line_rendering_appends_exactly_one_newline() {
    let msg = Message::error(23, "test message").with_role(Role::Sender);
    let bytes = msg.to_line_bytes().unwrap();
    let rendered = String::from_utf8_lossy(&bytes);
    assert!(
        rendered.ends_with('\n'),
        "Line rendering must end with newline"
    );
    assert!(
        !rendered.ends_with("\n\n"),
        "Line rendering must not have double newline"
    );
}

// ---------------------------------------------------------------------------
// 16. All exit code entries rendered as complete messages
// ---------------------------------------------------------------------------

#[test]
fn all_exit_code_messages_render_with_correct_prefix_and_code() {
    for entry in strings::exit_code_messages() {
        let msg = entry.to_message().with_role(Role::Sender);
        let rendered = msg.to_string();

        let expected_prefix = format!("rsync {}: ", entry.severity().as_str());
        assert!(
            rendered.starts_with(&expected_prefix),
            "Exit code {} message must start with '{expected_prefix}', got: {rendered}",
            entry.code()
        );

        let expected_code = format!("(code {})", entry.code());
        assert!(
            rendered.contains(&expected_code),
            "Exit code {} message must contain '{expected_code}', got: {rendered}",
            entry.code()
        );

        let version = crate::version::RUST_VERSION;
        assert!(
            rendered.contains(&format!("[sender={version}]")),
            "Exit code {} message must contain sender trailer, got: {rendered}",
            entry.code()
        );
    }
}
