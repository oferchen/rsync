//! What a daemon reached through a remote shell (`host::module` with `-e`)
//! writes to its `log file` about the peer.
//!
//! The fake remote shell sets the client variable sshd would set, so the
//! daemon sees a chosen peer address. Expected lines were measured against the
//! rsync 3.5.2 reference daemon.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// A one-file module whose daemon logs to `daemon.log`.
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        fs::create_dir(dir.path().join("mod")).expect("mkdir mod");
        fs::write(dir.path().join("mod/f"), "x\n").expect("write mod/f");
        let conf = format!(
            "log file = {}\nuse chroot = no\n[mod]\npath = {}\n",
            dir.path().join("daemon.log").display(),
            dir.path().join("mod").display()
        );
        fs::write(dir.path().join("rsyncd.conf"), conf).expect("write config");
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Lists the module through a remote shell that hands the daemon
    /// `SSH_CONNECTION` naming `peer`, or no client variable when `peer` is
    /// `None`, and returns the daemon's log.
    fn list_from(&self, peer: Option<&str>) -> String {
        let export = match peer {
            Some(addr) => {
                format!("SSH_CONNECTION='{addr} 40000 127.0.0.1 22'; export SSH_CONNECTION\n")
            }
            None => "unset SSH_CONNECTION\n".to_owned(),
        };
        let rsh = self.path("rsh.sh");
        fs::write(
            &rsh,
            format!(
                "#!/bin/sh\nshift\nunset REMOTE_HOST SSH_CLIENT SSH2_CLIENT\n{export}exec sh -c \"$*\"\n"
            ),
        )
        .expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        let output = Command::new(oc_rsync_bin())
            .arg("-e")
            .arg(&rsh)
            .arg(format!(
                "--rsync-path={} --config={}",
                oc_rsync_bin().display(),
                self.path("rsyncd.conf").display()
            ))
            .arg("--list-only")
            .arg("localhost::mod/")
            .output()
            .expect("run oc-rsync");
        assert!(
            output.status.success(),
            "listing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(self.path("daemon.log")).expect("read daemon log")
    }

    /// Starts the daemon side directly, as the remote shell would, with
    /// `SSH_CONNECTION` naming no valid address and `stdin` as fd 0. The
    /// session ends at once: the caller closes its end of stdin.
    fn daemon_with_unusable_peer(&self, stdin: Stdio) -> (Output, String) {
        let output = Command::new(oc_rsync_bin())
            .arg(format!("--config={}", self.path("rsyncd.conf").display()))
            .args(["--server", "--daemon", "."])
            .env_remove("REMOTE_HOST")
            .env_remove("SSH_CLIENT")
            .env_remove("SSH2_CLIENT")
            .env("SSH_CONNECTION", "bogus 40000 127.0.0.1 22")
            .stdin(stdin)
            .output()
            .expect("run oc-rsync daemon");
        let log = fs::read_to_string(self.path("daemon.log")).unwrap_or_default();
        (output, log)
    }
}

/// Index of the first log line containing `needle`.
fn line_index(log: &str, needle: &str) -> Option<usize> {
    log.lines().position(|line| line.contains(needle))
}

/// rsync 3.5.2 logs `rsync allowed access on module` only from a listening
/// daemon (clientserver.c:789-792, `am_daemon > 0`); over a remote shell the
/// log goes from `connect from` straight to the transfer line.
#[test]
fn access_line_is_not_logged_over_a_remote_shell() {
    let log = Fixture::new().list_from(Some("127.0.0.1"));
    assert!(line_index(&log, "connect from ").is_some(), "log:\n{log}");
    assert!(
        line_index(&log, "rsync on mod/ from ").is_some(),
        "log:\n{log}"
    );
    assert!(
        line_index(&log, "rsync allowed access on module").is_none(),
        "log:\n{log}"
    );
}

/// An address with no PTR record makes rsync 3.5.2's client_name() log
/// `name lookup failed for <addr>: <gai_strerror>` before the `connect from`
/// line names the host UNKNOWN (clientname.c:147-151). The reason is the
/// resolver's text, so only the part before it is pinned.
#[test]
fn unresolvable_peer_logs_the_name_lookup_failure_first() {
    let log = Fixture::new().list_from(Some("192.0.2.7"));
    let failure = line_index(&log, "name lookup failed for 192.0.2.7: ");
    let connect = line_index(&log, "connect from UNKNOWN (192.0.2.7)");
    assert!(
        matches!((failure, connect), (Some(f), Some(c)) if f < c),
        "log:\n{log}"
    );
}

/// With no client variable the address is 0.0.0.0, which rsync 3.5.2 maps
/// to UNKNOWN without a lookup or a log line (clientname.c:113-114).
#[test]
fn unaddressed_peer_logs_no_lookup_line() {
    let log = Fixture::new().list_from(None);
    assert!(
        line_index(&log, "connect from UNKNOWN (0.0.0.0)").is_some(),
        "log:\n{log}"
    );
    assert!(line_index(&log, "lookup").is_none(), "log:\n{log}");
}

/// A socketpair on fd 0 is an `AF_UNIX` peer, which upstream renders with
/// `getnameinfo(NI_NUMERICHOST)` (clientname.c:55-84, :319-327). The text is
/// whatever the C library writes - `localhost` on glibc, measured with rsync
/// 3.5.2 - or, if it refuses the family, the environment field the buffer
/// already held. `getaddrinfo(AI_NUMERICHOST)` then rejects it:
/// `malformed address <text>: <gai_strerror>` (clientname.c:120-123),
/// followed by `connect from UNKNOWN (<text>)`.
#[test]
fn socket_stdin_peer_is_read_from_the_socket() {
    use std::os::fd::AsFd;

    let fx = Fixture::new();
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let text = platform::peer_name::peer_numeric_host(ours.as_fd())
        .expect("getpeername on a socketpair")
        .unwrap_or_else(|| "bogus".to_owned());
    if cfg!(all(target_os = "linux", target_env = "gnu")) {
        assert_eq!(text, "localhost");
    }
    // Half-close rather than drop: the daemon reads EOF, but the socket stays
    // connected. A closed peer makes macOS `getpeername` fail with ENOTCONN,
    // which would test the pipe case instead.
    theirs
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close the client end");
    let (_output, log) =
        fx.daemon_with_unusable_peer(Stdio::from(std::os::fd::OwnedFd::from(ours)));
    drop(theirs);
    let malformed = line_index(&log, &format!("malformed address {text}: "));
    let connect = line_index(&log, &format!("connect from UNKNOWN ({text})"));
    assert!(
        matches!((malformed, connect), (Some(m), Some(c)) if m < c),
        "log:\n{log}"
    );
}

/// With a pipe on fd 0, as sshd hands a command, `getpeername` fails: upstream
/// logs `rsync: [Receiver] getpeername on fd0 failed: <strerror> (<errno>)`
/// and exits 10, writing the `error in socket IO` trailer to the log and to
/// stderr (clientname.c:323-327, log.c:1025-1051). The strerror text and errno
/// differ by platform, so only the parts before them are pinned.
#[test]
fn pipe_stdin_without_a_peer_address_exits_with_a_socket_error() {
    let fx = Fixture::new();
    let (output, log) = fx.daemon_with_unusable_peer(Stdio::piped());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(10), "stderr:\n{stderr}");
    let trailer = |text: &str| {
        text.lines().any(|line| {
            line.contains("error in socket IO (code 10)") && line.contains("[Receiver=")
        })
    };
    assert!(trailer(&stderr), "stderr:\n{stderr}");
    let cause = line_index(&log, "rsync: [Receiver] getpeername on fd0 failed: ");
    let logged_trailer = line_index(&log, "error in socket IO (code 10)");
    assert!(
        matches!((cause, logged_trailer), (Some(c), Some(t)) if c < t),
        "log:\n{log}"
    );
    assert!(trailer(&log), "log:\n{log}");
}
