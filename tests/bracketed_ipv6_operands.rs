//! Operands whose host is a bracketed IPv6 literal.
//!
//! upstream: options.c:3322-3373 `parse_hostspec()` skips the `[...]` of an
//! IPv6 literal, so the `::` inside `[::1]:path` is address text, not the
//! daemon separator, and the brackets are elided from the host
//! (options.c:3315-3320). main.c:1480-1535 then picks a remote shell for
//! `HOST:PATH` and a daemon for `HOST::PATH` or `rsync://`. oc searched the
//! raw operand for `::`, sent `[::1]:Music` down the daemon path and refused
//! it as an invalid bracketed daemon host.
//!
//! Every cell runs the client with `-e` naming a fake shell that records its
//! argv and closes the connection, so no network is used. The host token and
//! the command after it, and the exit code, are compared with rsync 3.5.2 when
//! it is installed, and with the values measured from it otherwise.
//!
//! oc also applies RFC 3986 section 3.2.2, which 3.5.2 does not: brackets may
//! hold only an IPv6 address. Those refusals are checked against oc alone.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(30);

/// A remote shell that records its argv. For a daemon it sends a greeting and
/// reads the client's greeting and module line before closing; otherwise it
/// reads one byte. Either way the client meets EOF at a fixed point, so the
/// exit code does not depend on scheduling.
const FAKE_SHELL: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$ARGV_OUT"
case " $* " in
*" --daemon "*)
  printf '@RSYNCD: 32.0 md5 md4\n'
  read -r greeting
  read -r module
  ;;
*) head -c 1 >/dev/null ;;
esac
exit 7
"#;

/// `connection unexpectedly closed` (RERR_STREAMIO).
const SHELL_CLOSED: i32 = 12;
/// `didn't get server startup line` (RERR_STARTCLIENT).
const DAEMON_CLOSED: i32 = 5;

struct Run {
    status: Option<i32>,
    stderr: String,
    /// The fake shell's argv, or `None` when the client never ran it.
    argv: Option<Vec<String>>,
}

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Runs `client` with the fake shell and `args`, from a scratch directory
/// holding `src/f` and an empty `dst/`.
fn run(client: &Path, args: &[&str]) -> Run {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    fs::write(root.join("src/f"), "x\n").unwrap();
    let shell = root.join("shell");
    fs::write(&shell, FAKE_SHELL).unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    let argv_out = root.join("argv");
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .env("ARGV_OUT", &argv_out)
        .arg("-e")
        .arg(&shell)
        .args(args);
    match run_deadlined(&mut command, BUDGET).unwrap() {
        Deadlined::Finished { status, stderr, .. } => Run {
            status: status.code(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            argv: fs::read_to_string(&argv_out)
                .ok()
                .map(|text| text.lines().map(str::to_owned).collect()),
        },
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "TIMEOUT: {args:?} still running after {budget:?}: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}

/// The valid bracketed spellings: `args`, the remote-shell argv 3.5.2 builds
/// (host token first, brackets elided), and its exit code.
const VALID: &[(&[&str], &[&str], i32)] = &[
    (
        &["-a", "[::1]:Music", "dst"],
        &[
            "::1",
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            ".",
            "Music",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-a", "user@[::1]:Music", "dst"],
        &[
            "-l",
            "user",
            "::1",
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            ".",
            "Music",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-a", "[::1]:Music/sub", "dst"],
        &[
            "::1",
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            ".",
            "Music/sub",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-a", "[::1]:", "dst"],
        &[
            "::1",
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            ".",
            ".",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-avP", "-6", "[fda4:d471:fb49:cb00::3]:Music", "zzz"],
        &[
            "fda4:d471:fb49:cb00::3",
            "rsync",
            "--server",
            "--sender",
            "-vlogDtpre.iLsfxCIvu",
            ".",
            "Music",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-a", "[fe80::1%lo]:x", "dst"],
        &[
            "fe80::1%lo",
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            ".",
            "x",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-a", "src/", "[::1]:Music"],
        &[
            "::1",
            "rsync",
            "--server",
            "-logDtpre.iLsfxCIvu",
            ".",
            "Music",
        ],
        SHELL_CLOSED,
    ),
    (
        &["-a", "[::1]::mod", "dst"],
        &["::1", "rsync", "--server", "--daemon", "."],
        DAEMON_CLOSED,
    ),
    (
        &["-a", "user@[::1]::mod", "dst"],
        &["-l", "user", "::1", "rsync", "--server", "--daemon", "."],
        DAEMON_CLOSED,
    ),
    (
        &["-a", "[fe80::1%lo]::mod", "dst"],
        &["fe80::1%lo", "rsync", "--server", "--daemon", "."],
        DAEMON_CLOSED,
    ),
    (
        &["-a", "rsync://[::1]:8873/mod", "dst"],
        &["::1", "rsync", "--server", "--daemon", "."],
        DAEMON_CLOSED,
    ),
];

fn assert_valid(label: &str, run: &Run, argv: &[&str], status: i32, args: &[&str]) {
    assert_eq!(
        run.argv.as_deref(),
        Some(&argv.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>()[..]),
        "{label} {args:?}: {}",
        run.stderr
    );
    assert_eq!(run.status, Some(status), "{label} {args:?}: {}", run.stderr);
}

/// Each valid spelling reaches the remote shell with the host token and
/// command 3.5.2 sends, and fails the way 3.5.2 does when the shell closes.
#[test]
fn bracketed_ipv6_operands_reach_the_remote_shell_like_upstream() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for &(args, argv, status) in VALID {
        if let Some(upstream) = &upstream {
            let measured = run(upstream.binary(), args);
            assert_valid("upstream", &measured, argv, status, args);
        }
        assert_valid("oc", &run(&oc_rsync(), args), argv, status, args);
    }
}

/// A `[` that does not open the host, an empty `[]`, or text after the `]`
/// makes no hostspec (options.c:3354-3360): the operand is a local path, so
/// no shell runs and the missing source ends the transfer with code 23.
#[test]
fn malformed_brackets_are_local_paths_like_upstream() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for operand in ["[::1]x:Music", "a[::1]:Music", "[]:Music"] {
        let args = ["-a", operand, "dst"];
        let mut clients = vec![("oc", oc_rsync())];
        if let Some(upstream) = &upstream {
            clients.push(("upstream", upstream.binary().to_path_buf()));
        }
        for (label, client) in clients {
            let measured = run(&client, &args);
            assert_eq!(measured.argv, None, "{label} {operand}: shell ran");
            assert_eq!(
                measured.status,
                Some(23),
                "{label} {operand}: {}",
                measured.stderr
            );
            assert!(
                measured.stderr.contains("link_stat"),
                "{label} {operand}: {}",
                measured.stderr
            );
        }
    }
}

const NOT_IPV6: &str = "Invalid remote host: only an IPv6 address may be enclosed in brackets.";
const BARE_IPV6: &str = "IPv6 daemon addresses must be enclosed in brackets";

/// Deliberate deviation from 3.5.2, which strips the brackets from any host
/// and connects to it, and sends an `rsync://` URL with a bare IPv6 host to
/// a remote shell on the host `rsync`: both are refused before any shell
/// runs, with RERR_SYNTAX.
#[test]
fn hosts_breaking_the_bracket_rule_are_refused() {
    for (operand, text) in [
        ("[1.2.3.4]:x", NOT_IPV6),
        ("[example.com]:x", NOT_IPV6),
        ("user@[garbage]:x", NOT_IPV6),
        ("[fe80::1%]:x", NOT_IPV6),
        ("[1.2.3.4]::mod", NOT_IPV6),
        ("[example.com]::mod", NOT_IPV6),
        ("rsync://[1.2.3.4]/mod", NOT_IPV6),
        ("rsync://[example.com]:8873/mod", NOT_IPV6),
        ("rsync://::1/mod", BARE_IPV6),
        ("rsync://fe80::1%lo/mod", BARE_IPV6),
    ] {
        let measured = run(&oc_rsync(), &["-a", operand, "dst"]);
        assert_eq!(measured.argv, None, "{operand}: shell ran");
        assert_eq!(measured.status, Some(1), "{operand}: {}", measured.stderr);
        assert!(
            measured.stderr.contains(text),
            "{operand}: {}",
            measured.stderr
        );
    }
}
