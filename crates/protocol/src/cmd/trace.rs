//! `--debug=CMD` producer emissions for remote command construction.
//!
//! Mirrors upstream rsync 3.4.4's `DEBUG_GTE(CMD, N)` output so wire-comparable
//! diagnostics align across implementations. All emissions use the upstream
//! quoting rules from `util1.c:print_child_argv` so that the printed argv
//! matches what upstream would write to `FCLIENT` byte-for-byte (modulo the
//! trailing newline which the [`logging`] sink supplies).
//!
//! # Upstream Reference
//!
//! - `util1.c:98-117` - `print_child_argv` prefix + quoted-argv + `(N args)`
//!   suffix used by every level-1 CMD emission.
//! - `pipe.c:54-55` - `print_child_argv("opening connection using:", command)`
//!   inside `piped_child()` before `fork()`/`execvp()`.
//! - `clientserver.c:348-349` - `print_child_argv("sending daemon args:", sargs)`
//!   immediately before writing the argument list to the daemon socket.
//! - `rsync.c:296-297` - `print_child_argv("protected args:", args + i + 1)`
//!   inside `send_protected_args()` before the per-arg iconv loop.
//! - `main.c:620-624` - per-argument enumeration `cmd[%d]=%s ...\n` emitted
//!   from `do_cmd()` once the final remote argv has been assembled.

use std::ffi::OsStr;

use logging::debug_log;

/// Characters upstream `print_child_argv` treats as safe-to-print unquoted.
///
/// upstream: util1.c:106-109 - the exact `strspn` whitelist used to decide
/// whether to wrap an argv element in double quotes.
const SAFE_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789,.-_=+@/";

/// Returns the argv element rendered with upstream's quoting rules.
///
/// Mirrors upstream `util1.c:103-113`: an element is wrapped in literal
/// double quotes whenever any byte falls outside the `SAFE_CHARS` whitelist;
/// otherwise the element is emitted verbatim. Non-UTF8 bytes are rendered
/// through [`String::from_utf8_lossy`] so the diagnostic remains printable.
#[must_use]
fn quote_arg(arg: &OsStr) -> String {
    let bytes = arg.to_string_lossy();
    let needs_quote = bytes.bytes().any(|b| !SAFE_CHARS.contains(&b));
    if needs_quote {
        format!("\"{bytes}\"")
    } else {
        bytes.into_owned()
    }
}

/// Renders an argv slice with the upstream `print_child_argv` shape.
///
/// Returns `"<prefix> arg1 arg2 ...  (N args)"`. The double space before
/// `(N args)` mirrors upstream `util1.c:101,112,116` which writes each
/// element with a trailing space and then prints the literal `" (%d args)\n"`.
///
/// upstream: util1.c:98-117 - reference implementation reused by every
/// level-1 CMD emission.
#[must_use]
pub fn print_child_argv<S: AsRef<OsStr>>(prefix: &str, argv: &[S]) -> String {
    let mut out = String::from(prefix);
    out.push(' ');
    for arg in argv {
        out.push_str(&quote_arg(arg.as_ref()));
        out.push(' ');
    }
    out.push(' ');
    out.push_str(&format!("({} args)", argv.len()));
    out
}

/// Traces the SSH command being spawned by the client (level 1).
///
/// upstream: pipe.c:54-55 - `print_child_argv("opening connection using:", command)`
/// in `piped_child()`. Emitted once per remote-shell child, capturing the
/// program name, every `-e`/`--rsh` option, the destination host, and the
/// final `rsync --server ...` argv.
#[inline]
pub fn trace_opening_connection<S: AsRef<OsStr>>(command: &[S]) {
    if logging::debug_gte(logging::DebugFlag::Cmd, 1) {
        let line = print_child_argv("opening connection using:", command);
        debug_log!(Cmd, 1, "{line}");
    }
}

/// Traces the daemon argument list being sent over the TCP socket (level 1).
///
/// upstream: clientserver.c:348-349 - `print_child_argv("sending daemon args:", sargs)`
/// inside `start_inband_exchange()` right before the per-arg write loop.
/// `sargs` includes the leading `--server [--sender] -flags . path` argv that
/// the daemon will hand to its own `parse_arguments()`.
#[inline]
pub fn trace_sending_daemon_args<S: AsRef<OsStr>>(sargs: &[S]) {
    if logging::debug_gte(logging::DebugFlag::Cmd, 1) {
        let line = print_child_argv("sending daemon args:", sargs);
        debug_log!(Cmd, 1, "{line}");
    }
}

/// Traces the protected (secluded) argument list being streamed over stdin (level 1).
///
/// upstream: rsync.c:296-297 - `print_child_argv("protected args:", args + i + 1)`
/// in `send_protected_args()` just before the per-arg `iconvbufs(ic_send, ...)`
/// loop. The argv passed to upstream begins after the original `NULL`
/// terminator (`args + i + 1`); callers should pass the same payload that
/// will be written to the peer.
#[inline]
pub fn trace_protected_args<S: AsRef<OsStr>>(args: &[S]) {
    if logging::debug_gte(logging::DebugFlag::Cmd, 1) {
        let line = print_child_argv("protected args:", args);
        debug_log!(Cmd, 1, "{line}");
    }
}

/// Traces the assembled remote argv element-by-element (level 2).
///
/// upstream: main.c:620-624 - inside `do_cmd()` once `args[]` has been
/// finalised:
///
/// ```text
/// for (i = 0; i < argc; i++)
///     rprintf(FCLIENT, "cmd[%d]=%s ", i, args[i]);
/// rprintf(FCLIENT, "\n");
/// ```
///
/// The emission is a single line of space-separated `cmd[i]=value` tokens
/// matching upstream's formatting (trailing space + `\n` rolled into the
/// `debug_log!` newline).
#[inline]
pub fn trace_cmd_argv<S: AsRef<OsStr>>(argv: &[S]) {
    if logging::debug_gte(logging::DebugFlag::Cmd, 2) {
        let mut line = String::new();
        for (i, arg) in argv.iter().enumerate() {
            let rendered = arg.as_ref().to_string_lossy();
            line.push_str(&format!("cmd[{i}]={rendered} "));
        }
        debug_log!(Cmd, 2, "{line}");
    }
}

#[cfg(test)]
mod tests {
    //! Pinning tests for CMD emission shapes. Strings match upstream
    //! `pipe.c`, `clientserver.c`, `rsync.c`, and `main.c` byte-for-byte
    //! once the `print_child_argv` helper from `util1.c` is taken into
    //! account.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
    use std::ffi::OsString;

    /// Initialises logging with the requested CMD level and clears the
    /// pending event buffer so assertions can focus on emissions produced
    /// by the test body.
    fn init_cmd(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.cmd = level;
        init(cfg);
        let _ = drain_events();
    }

    /// Collects CMD debug messages emitted since the last drain.
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

    /// Safe characters render without quotes.
    ///
    /// upstream: util1.c:106-113 - `strspn` against the safe-char set.
    #[test]
    fn quote_arg_passes_safe_argv_through() {
        assert_eq!(quote_arg(OsStr::new("--server")), "--server");
        assert_eq!(quote_arg(OsStr::new("user@host")), "user@host");
        assert_eq!(quote_arg(OsStr::new("/path/to/file")), "/path/to/file");
        assert_eq!(quote_arg(OsStr::new("a.b-c_d=1+2")), "a.b-c_d=1+2");
    }

    /// Whitespace, shell metacharacters, and colons trigger upstream's
    /// double-quote wrapping.
    ///
    /// upstream: util1.c:106-113 - any byte outside `SAFE_CHARS` forces
    /// the `"%s"` branch.
    #[test]
    fn quote_arg_wraps_unsafe_argv() {
        assert_eq!(quote_arg(OsStr::new("with space")), "\"with space\"");
        assert_eq!(quote_arg(OsStr::new("a:b")), "\"a:b\"");
        assert_eq!(quote_arg(OsStr::new("--rsh=ssh -p")), "\"--rsh=ssh -p\"");
    }

    /// An empty argv element renders without quotes, matching upstream.
    ///
    /// upstream: util1.c:106-113 - `strspn("", _) == strlen("") == 0`, so the
    /// `!=` test fails and the unquoted `%s` branch runs (which prints
    /// nothing, then the trailing space).
    #[test]
    fn quote_arg_empty_renders_unquoted() {
        assert_eq!(quote_arg(OsStr::new("")), "");
    }

    /// `print_child_argv` reproduces upstream `util1.c:98-117` formatting:
    /// prefix, space, quoted argv elements separated by single spaces, a
    /// double space, and `(N args)`.
    #[test]
    fn print_child_argv_matches_upstream_layout() {
        let argv = ["ssh", "user@host", "rsync", "--server", "."];
        let line = print_child_argv("opening connection using:", &argv);
        assert_eq!(
            line,
            "opening connection using: ssh user@host rsync --server .  (5 args)"
        );
    }

    /// An empty argv still emits the prefix, the double space, and `(0 args)`.
    ///
    /// upstream: util1.c:101 + util1.c:116 - prefix prints first, then the
    /// loop is skipped, then the `(N args)` literal closes the line.
    #[test]
    fn print_child_argv_with_empty_argv() {
        let argv: [&str; 0] = [];
        let line = print_child_argv("opening connection using:", &argv);
        assert_eq!(line, "opening connection using:  (0 args)");
    }

    /// Pins the level 1 `opening connection using:` emission for the
    /// upstream `pipe.c:54-55` site.
    #[test]
    fn opening_connection_matches_upstream_format() {
        init_cmd(1);
        let argv = ["ssh", "example.com", "rsync", "--server", "."];
        trace_opening_connection(&argv);
        let msgs = cmd_messages();
        assert!(
            msgs.iter().any(
                |m| m == "opening connection using: ssh example.com rsync --server .  (5 args)"
            ),
            "expected upstream-format CMD,1 opening-connection line, got {msgs:?}"
        );
    }

    /// Pins the level 1 `sending daemon args:` emission for the upstream
    /// `clientserver.c:348-349` site.
    #[test]
    fn sending_daemon_args_matches_upstream_format() {
        init_cmd(1);
        let argv = ["--server", "--sender", "-logDtpre.LsfxCIvu", ".", "/data"];
        trace_sending_daemon_args(&argv);
        let msgs = cmd_messages();
        assert!(
            msgs.iter().any(|m| m
                == "sending daemon args: --server --sender -logDtpre.LsfxCIvu . /data  (5 args)"),
            "expected upstream-format CMD,1 sending-daemon-args line, got {msgs:?}"
        );
    }

    /// Pins the level 1 `protected args:` emission for the upstream
    /// `rsync.c:296-297` site.
    #[test]
    fn protected_args_matches_upstream_format() {
        init_cmd(1);
        let argv = ["--server", "."];
        trace_protected_args(&argv);
        let msgs = cmd_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "protected args: --server .  (2 args)"),
            "expected upstream-format CMD,1 protected-args line, got {msgs:?}"
        );
    }

    /// Pins the level 2 `cmd[i]=value` enumeration for the upstream
    /// `main.c:620-624` site.
    #[test]
    fn trace_cmd_argv_matches_upstream_enumeration() {
        init_cmd(2);
        let argv = ["ssh", "host", "rsync", "--server", "."];
        trace_cmd_argv(&argv);
        let msgs = cmd_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "cmd[0]=ssh cmd[1]=host cmd[2]=rsync cmd[3]=--server cmd[4]=. "),
            "expected upstream-format CMD,2 enumeration line, got {msgs:?}"
        );
    }

    /// `OsString` argv elements are accepted directly without an explicit
    /// `to_str` conversion.
    #[test]
    fn helpers_accept_os_string_argv() {
        init_cmd(1);
        let argv: Vec<OsString> = vec!["ssh".into(), "host".into(), "rsync".into()];
        trace_opening_connection(&argv);
        let msgs = cmd_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "opening connection using: ssh host rsync  (3 args)"),
            "expected OsString-compatible opening-connection emission, got {msgs:?}"
        );
    }

    /// Level 0 suppresses every CMD emission, matching upstream's
    /// `DEBUG_GTE(CMD, _)` gate.
    #[test]
    fn level_zero_suppresses_all_cmd_emissions() {
        init_cmd(0);
        trace_opening_connection(&["ssh", "host"]);
        trace_sending_daemon_args(&["--server", "."]);
        trace_protected_args(&["--server", "."]);
        trace_cmd_argv(&["ssh", "host"]);
        assert!(
            cmd_messages().is_empty(),
            "all CMD emissions must be gated at level 0"
        );
    }

    /// Level 1 fires the three level-1 helpers but suppresses the level-2
    /// `cmd[i]=value` enumeration.
    #[test]
    fn level_one_gates_level_two_enumeration() {
        init_cmd(1);
        trace_cmd_argv(&["ssh", "host"]);
        assert!(
            cmd_messages().is_empty(),
            "level-2 enumeration must be gated at level 1"
        );
    }
}
