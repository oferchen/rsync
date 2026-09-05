// The daemon's per-request log line and the `request` value it names.
//
// upstream: clientserver.c:1207-1220 emits exactly one line per served
// request, gated on a non-empty `request`, once the argv is parsed and the
// pre-xfer hooks have run.

/// Ceiling upstream tests *before* appending each operand (`io.c:1487`).
///
/// The guard is `request_len < 1024`, evaluated before the append rather than
/// after it, so the assembled value can exceed 1024 by the length of the
/// operand that crossed the boundary. Truncating the result to 1024 would not
/// reproduce upstream's bytes.
const DAEMON_REQUEST_LEN_CAP: usize = 1024;

/// Assembles upstream's `request` from the client's argument vector.
///
/// `request` is *not* the module name. It is an out-param of `read_args()`
/// (`clientserver.c:1154`) built at `io.c:1486-1495` from the argv entries
/// that follow the `.` cwd marker, joined with single spaces:
///
/// - entries before the marker are options and never contribute;
/// - the marker itself never contributes, and only the first one switches
///   modes (`io.c:1507-1508` sets `dot_pos` from the *else* branch, which
///   stops running once `dot_pos` is set);
/// - the operands are the raw wire lines, before `glob_expand_module()`.
///
/// `None` when no operand followed the marker, which is upstream's `NULL`
/// `request` and suppresses the log line entirely.
fn daemon_request(client_args: &[String]) -> Option<String> {
    let mut past_marker = false;
    let mut request: Option<String> = None;
    let mut request_len = 0usize;

    for arg in client_args {
        if !past_marker {
            past_marker = arg == ".";
            continue;
        }
        if request_len >= DAEMON_REQUEST_LEN_CAP {
            continue;
        }
        let assembled = request.get_or_insert_with(String::new);
        if request_len != 0 {
            assembled.push(' ');
            request_len += 1;
        }
        assembled.push_str(arg);
        request_len += arg.len();
    }

    request
}

/// Renders upstream's per-request daemon log line.
///
/// upstream: clientserver.c:1208-1219 - two arms keyed on whether the session
/// authenticated, and a verb keyed on the daemon's role:
///
/// ```c
/// rprintf(FLOG, "rsync %s %s from %s@%s (%s)\n", am_sender ? "on" : "to", request, auth_user, host, addr);
/// rprintf(FLOG, "rsync %s %s from %s (%s)\n",    am_sender ? "on" : "to", request, host, addr);
/// ```
///
/// `am_sender` is the daemon serving a pull, which is [`ServerRole::Generator`]
/// here. The `[pid]` prefix the operator sees comes from the log-file stamp
/// (`log.c:122-132`), not from this format string.
///
/// An empty `auth_user` takes the anonymous arm: upstream branches on
/// `*auth_user`, and an unauthenticated session leaves that buffer empty.
fn daemon_request_log_line(
    request: &str,
    role: ServerRole,
    auth_user: Option<&str>,
    host_display: &str,
    peer_ip: IpAddr,
) -> String {
    let verb = match role {
        ServerRole::Generator => "on",
        ServerRole::Receiver => "to",
    };
    match auth_user.filter(|user| !user.is_empty()) {
        Some(user) => format!("rsync {verb} {request} from {user}@{host_display} ({peer_ip})"),
        None => format!("rsync {verb} {request} from {host_display} ({peer_ip})"),
    }
}

/// Emits the per-request line when the client named at least one operand.
///
/// upstream: clientserver.c:1207 - `if (request)`. A client that sent no
/// operand after the `.` marker produces no line at all.
fn log_daemon_request(
    ctx: &ModuleRequestContext<'_>,
    client_args: &[String],
    role: ServerRole,
    auth_user: Option<&str>,
) {
    let (Some(log), Some(request)) = (ctx.log_sink, daemon_request(client_args)) else {
        return;
    };
    let text = daemon_request_log_line(&request, role, auth_user, ctx.host_display(), ctx.peer_ip);
    log_message(log, &rsync_info!(text).with_role(Role::Daemon));
}

#[cfg(test)]
mod daemon_request_line_tests {
    //! Upstream parity for the per-request daemon log line
    //! (`clientserver.c:1207-1220`) and its `request` operand
    //! (`io.c:1486-1495`).

    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn peer() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))
    }

    /// The shape every `daemon-*` testsuite cell drives: options, the `.`
    /// marker, then one module-qualified operand.
    #[test]
    fn the_request_is_the_operand_after_the_dot_marker() {
        let argv = args(&["rsyncd", "--server", "--sender", "-e.LsfxCIu", ".", "mod/f"]);
        assert_eq!(daemon_request(&argv).as_deref(), Some("mod/f"));
    }

    /// upstream joins multiple operands with a single space (`io.c:1489-1490`).
    #[test]
    fn multiple_operands_join_with_single_spaces() {
        let argv = args(&["--server", ".", "mod/a", "mod/b", "mod/c"]);
        assert_eq!(daemon_request(&argv).as_deref(), Some("mod/a mod/b mod/c"));
    }

    /// A client that names no operand leaves upstream's `request` NULL, and
    /// `clientserver.c:1207` then writes nothing at all.
    #[test]
    fn no_operand_after_the_marker_yields_no_request() {
        assert_eq!(daemon_request(&args(&["--server", "--sender", "."])), None);
        assert_eq!(daemon_request(&args(&["--server", "--sender"])), None);
    }

    /// Only the first `.` switches modes; a later one is an ordinary operand.
    #[test]
    fn a_later_dot_is_an_operand_not_a_second_marker() {
        let argv = args(&["--server", ".", "mod/a", ".", "mod/b"]);
        assert_eq!(daemon_request(&argv).as_deref(), Some("mod/a . mod/b"));
    }

    /// The cap is tested BEFORE each append (`io.c:1487`), so the assembled
    /// value overshoots 1024 by the operand that crossed the boundary. A
    /// `truncate(1024)` would produce different bytes than upstream.
    #[test]
    fn the_cap_is_tested_before_the_append_so_the_result_overshoots() {
        let first = "a".repeat(DAEMON_REQUEST_LEN_CAP - 1);
        let second = "b".repeat(64);
        let third = "c".repeat(64);
        let argv = args(&["--server", ".", &first, &second, &third]);
        let request = daemon_request(&argv).expect("operands follow the marker");

        // first (1023) + separator (1) + second (64) = 1088; `third` is refused
        // because the cursor has reached the cap by the time it is considered.
        assert_eq!(request.len(), DAEMON_REQUEST_LEN_CAP - 1 + 1 + 64);
        assert!(request.ends_with(&second));
        assert!(!request.contains('c'));
    }

    /// upstream: clientserver.c:1213-1215 - the anonymous arm, `on` because a
    /// daemon serving a pull is `am_sender`.
    #[test]
    fn an_unauthenticated_pull_renders_upstreams_anonymous_arm() {
        assert_eq!(
            daemon_request_log_line(
                "mod/f",
                ServerRole::Generator,
                None,
                "client.example",
                peer()
            ),
            "rsync on mod/f from client.example (10.0.0.7)",
        );
    }

    /// upstream: clientserver.c:1209-1211 - the authenticated arm, `to` because
    /// a daemon serving a push is not `am_sender`.
    #[test]
    fn an_authenticated_push_renders_upstreams_user_arm() {
        assert_eq!(
            daemon_request_log_line(
                "mod/f",
                ServerRole::Receiver,
                Some("alice"),
                "client.example",
                peer()
            ),
            "rsync to mod/f from alice@client.example (10.0.0.7)",
        );
    }

    /// upstream branches on `*auth_user`, so an empty buffer takes the
    /// anonymous arm rather than emitting a bare `@`.
    #[test]
    fn an_empty_auth_user_takes_the_anonymous_arm() {
        assert_eq!(
            daemon_request_log_line("mod/f", ServerRole::Generator, Some(""), "host", peer()),
            "rsync on mod/f from host (10.0.0.7)",
        );
    }
}
