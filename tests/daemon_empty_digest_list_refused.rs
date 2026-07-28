//! An `@RSYNCD:` greeting that advertises an EMPTY digest list must be refused
//! with `RERR_UNSUPPORTED` (exit 4), not authenticated.
//!
//! Upstream splits the list off with
//! `daemon_auth_choices = strchr(buf + 9, ' ')` and keeps
//! `strdup(daemon_auth_choices + 1)` (clientserver.c:199-203). For
//! `@RSYNCD: 31.0 ` - well formed, one trailing space - that `strchr` succeeds,
//! so `daemon_auth_choices` is a **non-NULL empty string**.
//! `negotiate_daemon_auth()` (compat.c:849-882) therefore skips its no-list
//! substitute, `parse_negotiate_str()` returns 0 walking the empty list, and the
//! daemon answers
//! `@ERROR: your client does not support one of our daemon-auth checksums: <list>`
//! before calling `exit_cleanup(RERR_UNSUPPORTED)`.
//!
//! Ground truth, rsync 3.4.4 driven with a hand-written greeting:
//!
//! ```text
//! -> @RSYNCD: 31.0 \n   <- @ERROR: your client does not support one of our daemon-auth checksums: ...
//! -> @RSYNCD: 32.0 \n   <- @ERROR: your client does not support one of our daemon-auth checksums: ...
//! -> @RSYNCD: 31.0\n    <- @RSYNCD: AUTHREQD <challenge>
//! -> @RSYNCD: 32.0\n    <- @ERROR: your client omitted the digest name list: @RSYNCD: 32.0
//! ```
//!
//! The exit code is only observable from a single-session daemon: the TCP
//! listener deliberately keeps running after refusing one client, exactly as
//! upstream's parent survives a child's `exit_cleanup()`. So this test drives
//! the shipped binary in inetd mode - stdin is a real socket, which is upstream's
//! own `is_a_socket(STDIN_FILENO)` trigger (clientserver.c:1548) - and reads the
//! process exit status directly. A normal client never produces the trailing
//! space, so the greeting is written by hand.
//!
//! Unix-only: inetd mode needs stdin to be a socket, which requires handing a
//! `socketpair(2)` end to the child as fd 0.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

/// Wall-clock budget for one handshake. The exchange is four short lines, so
/// anything approaching this bound is a hang.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);

/// Upstream's `RERR_UNSUPPORTED`.
const RERR_UNSUPPORTED: i32 = 4;

/// The digest list this build advertises, echoed verbatim in the refusal.
const ADVERTISED_DIGESTS: &str = "sha512 sha256 sha1 md5 md4";

fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// Writes an `rsyncd.conf` with one auth-protected module and returns its path.
fn write_config(dir: &TempDir) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let module_dir = dir.path().join("module");
    std::fs::create_dir_all(&module_dir).expect("module dir");

    let secrets = dir.path().join("secrets");
    std::fs::write(&secrets, "alice:correctpassword\n").expect("write secrets");
    std::fs::set_permissions(&secrets, PermissionsExt::from_mode(0o600)).expect("chmod secrets");

    let config = dir.path().join("rsyncd.conf");
    std::fs::write(
        &config,
        format!(
            "[protected]\npath = {}\nauth users = alice\nsecrets file = {}\nuse chroot = false\n",
            module_dir.display(),
            secrets.display(),
        ),
    )
    .expect("write config");
    config
}

/// Runs one inetd-mode session against the shipped binary.
///
/// Sends `greeting` followed by a request for the `protected` module, and
/// returns the daemon's first answer line together with the process exit code.
///
/// `--no-detach` is mandatory: without it the daemon would background itself and
/// the assertions below would run against a process that had already exited.
fn run_session(greeting: &str) -> (String, Option<i32>) {
    let dir = TempDir::new().expect("temp dir");
    let config = write_config(&dir);

    // A socketpair end handed over as fd 0 is what makes the daemon take
    // upstream's `is_a_socket(STDIN_FILENO)` branch and serve exactly one
    // session over stdio.
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    theirs
        .set_read_timeout(Some(HANDSHAKE_DEADLINE))
        .expect("child read timeout");
    ours.set_read_timeout(Some(HANDSHAKE_DEADLINE))
        .expect("read timeout");
    ours.set_write_timeout(Some(HANDSHAKE_DEADLINE))
        .expect("write timeout");

    let child_stdin = Stdio::from(OwnedFd::from(theirs.try_clone().expect("clone for stdin")));
    let child_stdout = Stdio::from(OwnedFd::from(theirs));

    let mut child = Command::new(oc_rsync_binary())
        .arg("--daemon")
        .arg("--no-detach")
        .arg("--config")
        .arg(&config)
        .stdin(child_stdin)
        .stdout(child_stdout)
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let mut reader = BufReader::new(ours.try_clone().expect("clone for reading"));
    let mut writer = ours;

    let mut banner = String::new();
    reader.read_line(&mut banner).expect("server greeting");
    assert!(
        banner.starts_with("@RSYNCD: "),
        "expected a version banner, got: {banner:?}"
    );

    writer
        .write_all(greeting.as_bytes())
        .expect("send client greeting");
    writer
        .write_all(b"protected\n")
        .expect("send module request");
    writer.flush().expect("flush handshake");

    let mut answer = String::new();
    reader.read_line(&mut answer).expect("daemon answer");

    drop(reader);
    drop(writer);

    let status = child.wait().expect("await daemon");
    (answer.trim_end().to_owned(), status.code())
}

/// An empty advertised list is refused with upstream's exact wording and exit 4.
///
/// Both protocol levels are covered because they reach the refusal differently:
/// protocol 31 has no digest-list presence requirement at all, while protocol 32
/// has one that an empty list *satisfies* (the space is there), so both fall
/// through to `negotiate_daemon_auth()` and are refused there rather than by
/// `exchange_protocols()`.
#[test]
fn an_empty_digest_list_is_refused_with_exit_code_4() {
    for greeting in ["@RSYNCD: 31.0 \n", "@RSYNCD: 32.0 \n"] {
        let (answer, code) = run_session(greeting);

        assert_eq!(
            answer,
            format!(
                "@ERROR: your client does not support one of our daemon-auth checksums: \
                 {ADVERTISED_DIGESTS}"
            ),
            "greeting {greeting:?} advertises an EMPTY list and must be refused",
        );
        assert_eq!(
            code,
            Some(RERR_UNSUPPORTED),
            "greeting {greeting:?} must exit RERR_UNSUPPORTED",
        );
    }
}

/// The one-byte-different contrast: no space at all is an ABSENT list, which
/// upstream substitutes for and authenticates.
///
/// upstream: compat.c:857-862 - `daemon_auth_choices == NULL` selects
/// `protocol_version >= 30 ? "md5" : "md4"`. Protocol 31 is the newest version
/// that can reach it; at 32 the absent list is refused earlier, by
/// `exchange_protocols()`, with a different diagnostic.
#[test]
fn an_absent_digest_list_still_reaches_the_auth_challenge() {
    let (answer, _code) = run_session("@RSYNCD: 31.0\n");

    let challenge = answer
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .unwrap_or_else(|| panic!("an absent list must fall back, got: {answer}"));
    assert_eq!(
        challenge.len(),
        22,
        "the protocol-31 substitute is md5, whose unpadded base64 is 22 chars"
    );
}

/// At protocol 32 an absent list is refused by the presence gate instead, with
/// upstream's other diagnostic. Pinning it here keeps the two refusals from
/// being conflated when the empty-list case is fixed.
///
/// upstream: clientserver.c:203-211.
#[test]
fn an_absent_digest_list_past_protocol_31_is_refused_by_the_presence_gate() {
    let (answer, _code) = run_session("@RSYNCD: 32.0\n");

    assert_eq!(
        answer,
        "@ERROR: your client omitted the digest name list: @RSYNCD: 32.0",
    );
}
