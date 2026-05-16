//! Integration tests for `--debug=CMD` producer emissions exposed by the
//! `protocol::cmd` helper module.
//!
//! The trace helpers wrap upstream rsync 3.4.1's `print_child_argv`
//! formatting from `util1.c:98-117` and the four CMD emission sites at
//! `pipe.c:54`, `clientserver.c:348`, `rsync.c:296`, and `main.c:620`.
//! These tests drive each helper through the real [`logging`] channel - the
//! same path users hit when running `oc-rsync --debug=CMD`.

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use protocol::cmd::{
    print_child_argv, trace_cmd_argv, trace_opening_connection, trace_protected_args,
    trace_sending_daemon_args,
};

/// Initialises logging with the supplied CMD debug level and drains any
/// pending events so the per-test assertions can focus on emissions produced
/// by the test body itself.
fn init_cmd(level: u8) {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.cmd = level;
    init(cfg);
    let _ = drain_events();
}

/// Collects CMD-flagged debug messages emitted since the last drain.
fn cmd_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Cmd,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// Pure-formatter contract: `print_child_argv` reproduces upstream
/// `util1.c:98-117` layout exactly when given a typical SSH argv.
#[test]
fn print_child_argv_matches_upstream_pipe_invocation() {
    let argv = ["ssh", "user@example.com", "rsync", "--server", "."];
    assert_eq!(
        print_child_argv("opening connection using:", &argv),
        "opening connection using: ssh user@example.com rsync --server .  (5 args)"
    );
}

/// Pure-formatter contract: arguments containing whitespace are wrapped in
/// upstream-style double quotes via `util1.c:106-113`.
#[test]
fn print_child_argv_quotes_unsafe_args_like_upstream() {
    let argv = ["ssh", "-o", "ProxyJump=bastion host"];
    assert_eq!(
        print_child_argv("opening connection using:", &argv),
        "opening connection using: ssh -o \"ProxyJump=bastion host\"  (3 args)"
    );
}

/// `trace_opening_connection` fires the upstream-format `pipe.c:54-55`
/// emission at level 1 through the real logging channel.
#[test]
fn opening_connection_emits_under_debug_cmd_level_1() {
    init_cmd(1);
    let argv = ["ssh", "example.com", "rsync", "--server", "."];
    trace_opening_connection(&argv);
    let msgs = cmd_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "opening connection using: ssh example.com rsync --server .  (5 args)"),
        "expected CMD,1 opening-connection emission, got {msgs:?}"
    );
}

/// `trace_sending_daemon_args` fires the upstream-format
/// `clientserver.c:348-349` emission at level 1.
#[test]
fn sending_daemon_args_emits_under_debug_cmd_level_1() {
    init_cmd(1);
    let argv = ["--server", "--sender", "-logDtpre.LsfxCIvu", ".", "/data"];
    trace_sending_daemon_args(&argv);
    let msgs = cmd_messages();
    assert!(
        msgs.iter()
            .any(|m| m
                == "sending daemon args: --server --sender -logDtpre.LsfxCIvu . /data  (5 args)"),
        "expected CMD,1 sending-daemon-args emission, got {msgs:?}"
    );
}

/// `trace_protected_args` fires the upstream-format `rsync.c:296-297`
/// emission at level 1.
#[test]
fn protected_args_emits_under_debug_cmd_level_1() {
    init_cmd(1);
    let argv = ["--server", "."];
    trace_protected_args(&argv);
    let msgs = cmd_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "protected args: --server .  (2 args)"),
        "expected CMD,1 protected-args emission, got {msgs:?}"
    );
}

/// `trace_cmd_argv` fires the upstream-format `main.c:620-624` enumeration
/// only at level 2 or higher.
#[test]
fn cmd_argv_enumeration_emits_under_debug_cmd_level_2() {
    init_cmd(2);
    let argv = ["ssh", "host", "rsync", "--server", "."];
    trace_cmd_argv(&argv);
    let msgs = cmd_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "cmd[0]=ssh cmd[1]=host cmd[2]=rsync cmd[3]=--server cmd[4]=. "),
        "expected CMD,2 enumeration emission, got {msgs:?}"
    );
}

/// Level 1 must NOT trigger the level-2 `cmd[i]=value` enumeration; the
/// other three helpers fire at level 1 and the enumeration is gated.
#[test]
fn cmd_argv_enumeration_is_gated_at_level_1() {
    init_cmd(1);
    trace_cmd_argv(&["ssh", "host"]);
    assert!(
        cmd_messages().is_empty(),
        "level-2 enumeration must be silent at CMD,1"
    );
}

/// Level 0 suppresses every CMD emission, mirroring upstream's
/// `DEBUG_GTE(CMD, _)` gate.
#[test]
fn all_helpers_silent_under_debug_cmd_level_0() {
    init_cmd(0);
    trace_opening_connection(&["ssh", "host"]);
    trace_sending_daemon_args(&["--server", "."]);
    trace_protected_args(&["--server", "."]);
    trace_cmd_argv(&["ssh", "host"]);
    assert!(
        cmd_messages().is_empty(),
        "all CMD emissions must be silent at level 0"
    );
}
