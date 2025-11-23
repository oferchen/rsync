use std::env;
use std::ffi::OsString;
use std::io::Write;

use core::branding::Brand;
use core::fallback::CLIENT_FALLBACK_ENV;
use core::server::{ServerConfig, ServerRole};

use crate::frontend::server::{
    InvocationRole, ServerInvocation, daemon_mode_arguments, is_rsync_flag_string,
    is_rsync_flag_tail, run_server_mode, server_mode_requested, touch_server_invocation,
    write_server_error_message,
};

/// Simple in-memory writer that tracks written bytes and flush calls.
#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    flushes: usize,
}

impl Buffer {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            flushes: 0,
        }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    fn flushes(&self) -> usize {
        self.flushes
    }
}

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// daemon_mode_arguments
// -----------------------------------------------------------------------------

#[test]
fn daemon_mode_arguments_empty_args_returns_none() {
    let args: Vec<OsString> = Vec::new();
    assert!(daemon_mode_arguments(&args).is_none());
}

#[test]
fn daemon_mode_arguments_without_daemon_flag_returns_none() {
    let args = [
        OsString::from("rsync"),
        OsString::from("-av"),
        OsString::from("src"),
        OsString::from("dest"),
    ];
    assert!(daemon_mode_arguments(&args).is_none());
}

#[test]
fn daemon_mode_arguments_rewrites_program_name_for_upstream_rsync() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--daemon"),
        OsString::from("--no-detach"),
        OsString::from("--config=/etc/rsyncd.conf"),
    ];

    let rewritten =
        daemon_mode_arguments(&args).expect("daemon_mode_arguments should detect --daemon");
    let expected_daemon = Brand::Upstream.daemon_program_name();

    assert_eq!(rewritten[0], OsString::from(expected_daemon));
    assert_eq!(
        &rewritten[1..],
        &args[2..],
        "arguments after --daemon should be forwarded unchanged"
    );
}

#[test]
fn daemon_mode_arguments_rewrites_program_name_for_oc_brand() {
    let args = [
        OsString::from("oc-rsync"),
        OsString::from("--daemon"),
        OsString::from("--no-detach"),
        OsString::from("--config=/etc/oc-rsyncd/oc-rsyncd.conf"),
    ];

    let rewritten =
        daemon_mode_arguments(&args).expect("daemon_mode_arguments should detect --daemon");
    let expected_daemon = Brand::Oc.daemon_program_name();

    assert_eq!(rewritten[0], OsString::from(expected_daemon));
    assert_eq!(&rewritten[1..], &args[2..]);
}

#[test]
fn daemon_mode_arguments_preserves_double_dash_and_subsequent_args() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--daemon"),
        OsString::from("--no-detach"),
        OsString::from("--"),
        OsString::from("--not-a-daemon-flag"),
    ];

    let rewritten =
        daemon_mode_arguments(&args).expect("daemon_mode_arguments should detect --daemon");

    assert_eq!(
        rewritten.len(),
        4,
        "should contain daemon bin, --no-detach, --, and trailing arg"
    );
    assert_eq!(rewritten[1], OsString::from("--no-detach"));
    assert_eq!(rewritten[2], OsString::from("--"));
    assert_eq!(rewritten[3], OsString::from("--not-a-daemon-flag"));
}

#[test]
fn daemon_mode_arguments_ignores_daemon_flag_after_double_dash() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--no-detach"),
        OsString::from("--"),
        OsString::from("--daemon"),
    ];

    assert!(daemon_mode_arguments(&args).is_none());
}

// -----------------------------------------------------------------------------
// server_mode_requested
// -----------------------------------------------------------------------------

#[test]
fn server_mode_requested_detects_flag_before_double_dash() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
    ];

    assert!(server_mode_requested(&args));
}

#[test]
fn server_mode_requested_ignores_flag_after_double_dash() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--"),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
    ];

    assert!(!server_mode_requested(&args));
}

#[test]
fn server_mode_requested_is_false_when_flag_absent() {
    let args = [
        OsString::from("rsync"),
        OsString::from("-av"),
        OsString::from("src"),
        OsString::from("dest"),
    ];
    assert!(!server_mode_requested(&args));
}

// -----------------------------------------------------------------------------
// ServerInvocation::parse and related helper methods
// -----------------------------------------------------------------------------

#[test]
fn parses_receiver_invocation_and_normalises_dot_placeholder() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("."),
        OsString::from("dest"),
    ];

    let invocation = ServerInvocation::parse(&args).expect("invocation parses");
    assert_eq!(invocation.role, InvocationRole::Receiver);
    assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
    assert_eq!(invocation.args, vec![OsString::from("dest")]);

    let config: ServerConfig = invocation.into_server_config().expect("config parses");
    assert_eq!(config.role, ServerRole::Receiver);
    assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
    assert_eq!(config.args, vec![OsString::from("dest")]);
}

#[test]
fn parses_receiver_invocation_without_dot_placeholder() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("dest"),
    ];

    let invocation = ServerInvocation::parse(&args).expect("invocation parses");
    assert_eq!(invocation.role, InvocationRole::Receiver);
    assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
    assert_eq!(invocation.args, vec![OsString::from("dest")]);

    let config: ServerConfig = invocation.into_server_config().expect("config parses");
    assert_eq!(config.role, ServerRole::Receiver);
    assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
    assert_eq!(config.args, vec![OsString::from("dest")]);
}

#[test]
fn parses_receiver_invocation_with_split_flag_block() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-l"),
        OsString::from("ogDtpre.iLsfxC"),
        OsString::from("."),
        OsString::from("dest"),
    ];

    let invocation = ServerInvocation::parse(&args).expect("invocation parses");
    assert_eq!(invocation.role, InvocationRole::Receiver);
    assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
    assert_eq!(invocation.args, vec![OsString::from("dest")]);

    let config: ServerConfig = invocation.into_server_config().expect("config parses");
    assert_eq!(config.role, ServerRole::Receiver);
    assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
    assert_eq!(config.args, vec![OsString::from("dest")]);
}

#[test]
fn parses_sender_invocation_without_placeholder() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("relative"),
        OsString::from("dest"),
    ];

    let invocation = ServerInvocation::parse(&args).expect("invocation parses");
    assert_eq!(invocation.role, InvocationRole::Generator);
    assert_eq!(invocation.raw_flag_string, "-logDtpre.iLsfxC");
    assert_eq!(
        invocation.args,
        vec![OsString::from("relative"), OsString::from("dest")]
    );

    let config: ServerConfig = invocation.into_server_config().expect("config parses");
    assert_eq!(config.role, ServerRole::Generator);
    assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
    assert_eq!(
        config.args,
        vec![OsString::from("relative"), OsString::from("dest")]
    );
}

#[test]
fn parse_rejects_missing_program_name() {
    let args: [OsString; 0] = [];
    let error = ServerInvocation::parse(&args).expect_err("parse should fail");
    assert!(
        error.contains("missing program name"),
        "unexpected error: {error}"
    );
}

#[test]
fn parse_rejects_missing_server_marker() {
    let args = [
        OsString::from("rsync"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("dest"),
    ];
    let error = ServerInvocation::parse(&args).expect_err("parse should fail");
    assert!(
        error.contains("expected --server"),
        "unexpected error: {error}"
    );
}

#[test]
fn parse_rejects_missing_flag_string() {
    let args = [OsString::from("rsync"), OsString::from("--server")];
    let error = ServerInvocation::parse(&args).expect_err("parse should fail");
    assert!(error.contains("flag string"), "unexpected error: {error}");
}

#[test]
fn parse_rejects_invalid_flag_block() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-/notflags"),
        OsString::from("dest"),
    ];
    let error = ServerInvocation::parse(&args).expect_err("parse should fail");
    assert!(
        error.contains("invalid rsync server flag string"),
        "unexpected error: {error}"
    );
}

#[test]
fn parse_rejects_missing_server_arguments() {
    let args = [
        OsString::from("rsync"),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
    ];
    let error = ServerInvocation::parse(&args).expect_err("parse should fail");
    assert!(
        error.contains("missing server arguments"),
        "unexpected error: {error}"
    );
}

#[test]
fn parse_flag_block_accepts_combined_and_split_forms() {
    let (combined, next) =
        ServerInvocation::parse_flag_block(&[OsString::from("-logDtpre.iLsfxC")], 0)
            .expect("combined form is valid");
    assert_eq!(combined, "-logDtpre.iLsfxC");
    assert_eq!(next, 1);

    let args = [
        OsString::from("-l"),
        OsString::from("ogDtpre.iLsfxC"),
        OsString::from("ignored"),
    ];
    let (split, next) = ServerInvocation::parse_flag_block(&args, 0).expect("split form is valid");
    assert_eq!(split, "-logDtpre.iLsfxC");
    assert_eq!(next, 2);
}

#[test]
fn parse_flag_block_rejects_invalid_tail() {
    let args = [OsString::from("-l"), OsString::from("invalid/tail")];
    let err = ServerInvocation::parse_flag_block(&args, 0).expect_err("parse_flag_block must fail");
    assert!(
        err.contains("invalid rsync server flag string"),
        "unexpected error: {err}"
    );
}

// -----------------------------------------------------------------------------
// Flag validation helpers
// -----------------------------------------------------------------------------

#[test]
fn is_rsync_flag_string_accepts_valid_block() {
    assert!(is_rsync_flag_string("-logDtpre.iLsfxC"));
    assert!(is_rsync_flag_string("-a"));
    assert!(is_rsync_flag_string("-Z123._+,"));
}

#[test]
fn is_rsync_flag_string_rejects_invalid_block() {
    assert!(!is_rsync_flag_string("logDtpre.iLsfxC"));
    assert!(!is_rsync_flag_string("-"));
    assert!(!is_rsync_flag_string("-logD/tre"));
}

#[test]
fn is_rsync_flag_tail_accepts_valid_tail() {
    assert!(is_rsync_flag_tail("ogDtpre.iLsfxC"));
    assert!(is_rsync_flag_tail("a"));
    assert!(is_rsync_flag_tail("Z123._+,"));
}

#[test]
fn is_rsync_flag_tail_rejects_invalid_tail() {
    assert!(!is_rsync_flag_tail(""));
    assert!(!is_rsync_flag_tail("with/slash"));
}

// -----------------------------------------------------------------------------
// Error reporting and façade behaviour
// -----------------------------------------------------------------------------

#[test]
fn write_server_error_message_writes_rsync_style_error() {
    let mut stderr = Buffer::new();
    write_server_error_message(
        &mut stderr,
        Brand::Oc,
        "server mode is not yet implemented for role Receiver",
    );

    let text = stderr.as_str();
    assert!(
        text.contains("server mode is not yet implemented for role Receiver"),
        "stderr did not contain expected error, got: {text}"
    );
    assert!(
        text.to_ascii_lowercase().contains("daemon"),
        "error message should indicate daemon / server role: {text}"
    );
}

#[test]
fn run_server_mode_reports_unimplemented_receiver_role_and_flushes_streams() {
    let args = [
        OsString::from("oc-rsync"),
        OsString::from("--server"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("."),
        OsString::from("dest"),
    ];

    let mut stdout = Buffer::new();
    let mut stderr = Buffer::new();

    env::remove_var(CLIENT_FALLBACK_ENV);

    let code = run_server_mode(&args, &mut stdout, &mut stderr);
    assert_eq!(code, 1);

    assert!(
        stdout.flushes() >= 1,
        "stdout should have been flushed at least once"
    );
    assert!(
        stderr.flushes() >= 1,
        "stderr should have been flushed at least once"
    );

    let text = stderr.as_str();
    assert!(
        text.contains("server mode is not yet implemented for role Receiver"),
        "stderr should mention receiver role being unimplemented; got: {text}"
    );
}

#[test]
fn run_server_mode_reports_unimplemented_generator_role() {
    let args = [
        OsString::from("oc-rsync"),
        OsString::from("--server"),
        OsString::from("--sender"),
        OsString::from("-logDtpre.iLsfxC"),
        OsString::from("relative"),
        OsString::from("dest"),
    ];

    let mut stdout = Buffer::new();
    let mut stderr = Buffer::new();

    env::remove_var(CLIENT_FALLBACK_ENV);

    let code = run_server_mode(&args, &mut stdout, &mut stderr);
    assert_eq!(code, 1);

    let text = stderr.as_str();
    assert!(
        text.contains("server mode is not yet implemented for role Generator"),
        "stderr should mention generator role being unimplemented; got: {text}"
    );
}

#[test]
fn run_server_mode_reports_parse_error_for_invalid_invocation() {
    let args = [OsString::from("oc-rsync"), OsString::from("--server")];

    let mut stdout = Buffer::new();
    let mut stderr = Buffer::new();

    env::remove_var(CLIENT_FALLBACK_ENV);

    let code = run_server_mode(&args, &mut stdout, &mut stderr);
    assert_eq!(code, 1);

    let text = stderr.as_str();
    assert!(
        text.contains("flag string"),
        "stderr should mention flag string parse error; got: {text}"
    );
}

// -----------------------------------------------------------------------------
// Sanity: touch_server_invocation helper
// -----------------------------------------------------------------------------

#[test]
fn touch_server_invocation_is_noop_but_compiles() {
    let invocation = ServerInvocation {
        role: InvocationRole::Receiver,
        raw_flag_string: "-logDtpre.iLsfxC".to_string(),
        args: vec![OsString::from("dest")],
    };

    touch_server_invocation(&invocation);
}
