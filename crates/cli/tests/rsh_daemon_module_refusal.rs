//! A daemon started through a remote shell (`--server --daemon .`) that
//! refuses the requested module exits like rsync 3.5.2.
//!
//! upstream: every refusal `rsync_module()` answers with an `@ERROR` line is
//! followed by `return -1` - hosts allow/deny (clientserver.c:787), the
//! connection limit and its lock file (:806), authentication (:817), an
//! invalid uid or gid (:842, :864), a missing `path` (:885), the module
//! directory (:905-921, :1057, :1064) and `early exec` (:1012).
//! `start_daemon()` and `main()` (main.c:1978) pass that on, so the process
//! exits 255 with nothing on stderr. After `@RSYNCD: OK` a refused option or a
//! failed `pre-xfer exec` ends in `exit_cleanup(RERR_UNSUPPORTED)`
//! (clientserver.c:1276), exit 4, and a push into a read-only module in
//! `exit_cleanup(RERR_SYNTAX)` (main.c:1203), exit 1.
//!
//! Every cell asserts the 3.5.2 result measured on glibc. When the 3.5.2
//! oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs 3.5.2 on the same input
//! and compares with it.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// What fd 0 is: a socket, as some remote shells hand over, or a pipe, as
/// sshd does.
#[derive(Clone, Copy, Debug)]
enum Stdin {
    Socketpair,
    Pipe,
}

const GREETING: &str = "@RSYNCD: 32.0 md5\n";

/// A refusal written before `@RSYNCD: OK`.
struct Refusal {
    name: &'static str,
    /// Global parameters; `{dir}` is the fixture directory.
    global: &'static str,
    /// The `[mod]` section; `{dir}` is the fixture directory.
    module: &'static str,
    /// What the client sends after its greeting.
    input: &'static str,
    /// The daemon's output after its greeting, with the authentication
    /// challenge replaced by `<challenge>`.
    reply: &'static str,
    /// The cell needs `chroot()` to fail, which it does not for root.
    needs_unprivileged: bool,
}

const fn refusal(
    name: &'static str,
    global: &'static str,
    module: &'static str,
    input: &'static str,
    reply: &'static str,
) -> Refusal {
    Refusal {
        name,
        global,
        module,
        input,
        reply,
        needs_unprivileged: false,
    }
}

const AUTH_MODULE: &str = "path = {dir}/mod\nauth users = u\nsecrets file = {dir}/secrets\n";
const AUTH_FAILED: &str = "@RSYNCD: AUTHREQD <challenge>\n@ERROR: auth failed on module mod\n";

const REFUSALS: [Refusal; 14] = [
    refusal(
        "hosts deny",
        "",
        "path = {dir}/mod\nhosts deny = *\n",
        "mod\n",
        "@ERROR: access denied to mod from UNDETERMINED (127.0.0.1)\n",
    ),
    refusal(
        "hosts allow without the client",
        "",
        "path = {dir}/mod\nhosts allow = 10.9.9.9\n",
        "mod\n",
        "@ERROR: access denied to mod from UNDETERMINED (127.0.0.1)\n",
    ),
    refusal(
        "hosts deny on an unlisted module",
        "",
        "path = {dir}/mod\nhosts deny = *\nlist = no\n",
        "mod\n",
        "@ERROR: Unknown module 'mod'\n",
    ),
    refusal(
        "lock file that cannot be opened",
        "lock file = {dir}/missing/daemon.lock\n",
        "path = {dir}/mod\nmax connections = 1\n",
        "mod\n",
        "@ERROR: failed to open lock file\n",
    ),
    refusal(
        "wrong authentication response",
        "",
        AUTH_MODULE,
        "mod\nu AAAAAAAAAAAAAAAAAAAAAA\n",
        AUTH_FAILED,
    ),
    refusal(
        "unknown authentication user",
        "",
        AUTH_MODULE,
        "mod\nx AAAAAAAAAAAAAAAAAAAAAA\n",
        AUTH_FAILED,
    ),
    refusal(
        "client closes at the challenge",
        "",
        AUTH_MODULE,
        "mod\n",
        AUTH_FAILED,
    ),
    refusal(
        "invalid uid",
        "",
        "path = {dir}/mod\nuid = nosuchuser_zz\n",
        "mod\n",
        "@ERROR: invalid uid nosuchuser_zz\n",
    ),
    refusal(
        "invalid gid",
        "",
        "path = {dir}/mod\ngid = nosuchgroup_zz\n",
        "mod\n",
        "@ERROR: invalid gid nosuchgroup_zz\n",
    ),
    refusal(
        "no path",
        "",
        "comment = no path here\n",
        "mod\n",
        "@ERROR: no path setting.\n",
    ),
    refusal(
        "missing path without chroot",
        "",
        "path = {dir}/missing\n",
        "mod\n",
        "@ERROR: chdir failed\n",
    ),
    refusal(
        "missing path with chroot",
        "",
        "path = {dir}/missing\nuse chroot = yes\n",
        "mod\n",
        "@ERROR: chroot failed\n",
    ),
    Refusal {
        name: "chroot without the privilege",
        global: "",
        module: "path = {dir}/mod\nuse chroot = yes\n",
        input: "mod\n",
        reply: "@ERROR: chroot failed\n",
        needs_unprivileged: true,
    },
    refusal(
        "early exec that fails",
        "",
        "path = {dir}/mod\nearly exec = false\n",
        "mod\n",
        "@ERROR: early exec failed\n",
    ),
];

/// The client's half of the checksum negotiation: one name, length-prefixed.
const NEGOTIATION: &str = "\x03md5";

/// A refusal made after `@RSYNCD: OK`: the `[mod]` section, the server
/// arguments the client sends, and the exit code.
///
/// Only the exit code is compared. rsync 3.5.2 writes these messages to the
/// daemon's own stderr; oc sends them to the client in the stream.
const POST_HANDSHAKE: [(&str, &str, &str, i32); 3] = [
    (
        "refused option",
        "path = {dir}/mod\nrefuse options = checksum\n",
        "--server\0--sender\0-rce.LsfxCIvu\0.\0mod/\0\0",
        4,
    ),
    (
        "pre-xfer exec that fails",
        "path = {dir}/mod\npre-xfer exec = false\n",
        "--server\0--sender\0-re.LsfxCIvu\0.\0mod/\0\0",
        4,
    ),
    (
        "push into a read-only module",
        "path = {dir}/mod\n",
        "--server\0-re.LsfxCIvu\0.\0mod/\0\0",
        1,
    ),
];

/// A daemon configuration with one module, `mod`.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(global: &str, module: &str) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().display().to_string();
        fs::create_dir(dir.path().join("mod")).expect("mkdir mod");
        let secrets = dir.path().join("secrets");
        fs::write(&secrets, "u:pw\n").expect("write secrets");
        fs::set_permissions(&secrets, fs::Permissions::from_mode(0o600)).expect("chmod secrets");
        let use_chroot = if module.contains("use chroot") {
            ""
        } else {
            "use chroot = no\n"
        };
        fs::write(
            dir.path().join("rsyncd.conf"),
            format!("{use_chroot}reverse lookup = no\n{global}[mod]\n{module}")
                .replace("{dir}", &root),
        )
        .expect("write config");
        Self { dir }
    }

    fn is_root(&self) -> bool {
        fs::metadata(self.dir.path()).expect("stat tempdir").uid() == 0
    }

    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .arg(format!(
                "--config={}",
                self.dir.path().join("rsyncd.conf").display()
            ))
            .args(["--server", "--daemon", "."])
            .current_dir(self.dir.path())
            .env_remove("REMOTE_HOST")
            .env_remove("SSH_CLIENT")
            .env_remove("SSH2_CLIENT")
            .env("SSH_CONNECTION", "127.0.0.1 40000 127.0.0.1 22")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Runs a session that receives `input` and then sees the client close.
    fn run(&self, binary: &Path, stdin: Stdin, input: &[u8]) -> Output {
        let mut command = self.command(binary);
        match stdin {
            Stdin::Socketpair => {
                let (ours, mut theirs) = UnixStream::pair().expect("socketpair");
                theirs.write_all(input).expect("send client bytes");
                // Half-close: the daemon reads EOF after the input, while its
                // replies still have a reader until it exits.
                theirs
                    .shutdown(std::net::Shutdown::Write)
                    .expect("half-close the client end");
                let output = command
                    .stdin(Stdio::from(std::os::fd::OwnedFd::from(ours)))
                    .output()
                    .expect("run daemon");
                drop(theirs);
                output
            }
            Stdin::Pipe => {
                let mut child = command.stdin(Stdio::piped()).spawn().expect("spawn daemon");
                let mut pipe = child.stdin.take().expect("daemon stdin");
                pipe.write_all(input).expect("send client bytes");
                drop(pipe);
                child.wait_with_output().expect("wait for daemon")
            }
        }
    }

    /// Starts a session that selects `mod` and keeps its connection slot: it
    /// returns once the daemon has answered `@RSYNCD: OK`, which it does only
    /// after claiming the slot, and the session then waits for arguments. The
    /// flag says whether that answer arrived.
    fn hold_a_slot(&self, binary: &Path) -> (Child, bool) {
        let mut child = self
            .command(binary)
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn daemon");
        child
            .stdin
            .as_mut()
            .expect("daemon stdin")
            .write_all(format!("{GREETING}mod\n").as_bytes())
            .expect("send client bytes");
        let accepted = BufReader::new(child.stdout.as_mut().expect("daemon stdout"))
            .lines()
            .map_while(Result::ok)
            .any(|line| line == "@RSYNCD: OK");
        (child, accepted)
    }
}

/// The daemon's reply after its own greeting line, with the random
/// authentication challenge masked.
fn reply(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (greeting, rest) = stdout.split_once('\n').expect("a greeting line");
    assert!(greeting.starts_with("@RSYNCD: "), "{output:?}");
    match rest.strip_prefix("@RSYNCD: AUTHREQD ") {
        Some(after) => {
            let (_, tail) = after.split_once('\n').expect("a challenge line");
            format!("@RSYNCD: AUTHREQD <challenge>\n{tail}")
        }
        None => rest.to_owned(),
    }
}

/// Asserts that a refused session exits 255 with `expected_reply` and an
/// empty stderr, and that rsync 3.5.2 does the same.
fn assert_aborted(oc: &Output, reference: Option<&Output>, expected_reply: &str, cell: &str) {
    assert_eq!(oc.status.code(), Some(255), "{cell}: {oc:?}");
    assert_eq!(reply(oc), expected_reply, "{cell}: {oc:?}");
    assert!(oc.stderr.is_empty(), "{cell}: {oc:?}");
    if let Some(reference) = reference {
        assert_eq!(
            (oc.status.code(), reply(oc), &oc.stderr),
            (reference.status.code(), reply(reference), &reference.stderr),
            "{cell}: oc differs from rsync 3.5.2"
        );
    }
}

fn check_refusals(stdin: Stdin) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for cell in &REFUSALS {
        let fixture = Fixture::new(cell.global, cell.module);
        if cell.needs_unprivileged && fixture.is_root() {
            continue;
        }
        let input = format!("{GREETING}{}", cell.input);
        let oc = fixture.run(&oc_rsync_bin(), stdin, input.as_bytes());
        let reference = upstream
            .as_ref()
            .map(|upstream| fixture.run(upstream.binary(), stdin, input.as_bytes()));
        assert_aborted(
            &oc,
            reference.as_ref(),
            cell.reply,
            &format!("{stdin:?} / {}", cell.name),
        );
    }
}

fn check_connection_limit(stdin: Stdin) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    let input = format!("{GREETING}mod\n");
    let second_session = |binary: &Path| {
        let fixture = Fixture::new(
            "lock file = {dir}/daemon.lock\n",
            "path = {dir}/mod\nmax connections = 1\n",
        );
        let (mut first, accepted) = fixture.hold_a_slot(binary);
        let output = fixture.run(binary, stdin, input.as_bytes());
        drop(first.stdin.take());
        first.wait().expect("wait for the first session");
        assert!(accepted, "the first session was not accepted");
        output
    };
    let oc = second_session(&oc_rsync_bin());
    let reference = upstream
        .as_ref()
        .map(|upstream| second_session(upstream.binary()));
    assert_aborted(
        &oc,
        reference.as_ref(),
        "@ERROR: max connections (1) reached -- try again later\n",
        &format!("{stdin:?} / connection limit reached"),
    );
}

fn check_post_handshake(stdin: Stdin) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (name, module, arguments, expected) in POST_HANDSHAKE {
        let fixture = Fixture::new("", module);
        let input = format!("{GREETING}mod\n{arguments}{NEGOTIATION}");
        let cell = format!("{stdin:?} / {name}");
        let oc = fixture.run(&oc_rsync_bin(), stdin, input.as_bytes());
        assert_eq!(oc.status.code(), Some(expected), "{cell}: {oc:?}");
        if let Some(upstream) = upstream.as_ref() {
            let reference = fixture.run(upstream.binary(), stdin, input.as_bytes());
            assert_eq!(
                oc.status.code(),
                reference.status.code(),
                "{cell}: oc differs from rsync 3.5.2: {reference:?}"
            );
        }
    }
}

#[test]
fn socket_stdin_module_refusal_exits_255() {
    check_refusals(Stdin::Socketpair);
}

#[test]
fn pipe_stdin_module_refusal_exits_255() {
    check_refusals(Stdin::Pipe);
}

#[test]
fn socket_stdin_connection_limit_exits_255() {
    check_connection_limit(Stdin::Socketpair);
}

#[test]
fn pipe_stdin_connection_limit_exits_255() {
    check_connection_limit(Stdin::Pipe);
}

#[test]
fn socket_stdin_post_handshake_refusal_exits_with_its_code() {
    check_post_handshake(Stdin::Socketpair);
}

#[test]
fn pipe_stdin_post_handshake_refusal_exits_with_its_code() {
    check_post_handshake(Stdin::Pipe);
}
