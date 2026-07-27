//! The client must ABORT when the daemon's digest list names nothing it
//! implements - never fall back to a protocol-keyed default.
//!
//! upstream: `auth_client()` calls `negotiate_daemon_auth(f_out, 1)`, whose
//! client branch runs `recv_negotiate_str()` over the server's list
//! (compat.c:865-869). When `parse_negotiate_str()` finds no mutual name it
//! falls through to compat.c:383-406, which prints
//!
//! ```text
//! Failed to negotiate a daemon auth checksum choice.
//! Server list: <what the server advertised>
//! Client list: <what we advertise>
//! ```
//!
//! and calls `exit_cleanup(RERR_UNSUPPORTED)`. Only an *absent* list gets the
//! `protocol_version >= 30 ? "md5" : "md4"` substitute (compat.c:857-862).
//!
//! Falling back here is worse than a wrong exit code: it puts a hash on the wire
//! that upstream never sends. Below protocol 30 the substitute is the seeded
//! `CSUM_MD4_OLD` (checksum.c:604-610), so the client would answer a challenge
//! it should have refused, with an algorithm the peer never named.
//!
//! Ground truth, rsync 3.4.4 against a stub server advertising `bogus1 bogus2`:
//!
//! ```text
//! Failed to negotiate a daemon auth checksum choice.
//! Server list: bogus1 bogus2
//! Client list: sha512 sha256 sha1 md5 md4
//! rsync error: requested action not supported (code 4) at compat.c(406) [Receiver=3.4.4]
//! ```
//!
//! The stub server is hand-written: a real daemon never advertises a list it
//! cannot honour, and only the raw socket lets the greeting be controlled.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Wall-clock budget for one client run. The exchange is a handful of short
/// lines, so anything near this bound is a hang.
const CLIENT_DEADLINE: Duration = Duration::from_secs(30);

/// Upstream's `RERR_UNSUPPORTED`.
const RERR_UNSUPPORTED: i32 = 4;

fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// Serves exactly one connection, greeting with `banner` and then answering the
/// module request with an auth challenge.
///
/// The challenge is what forces the client to negotiate: upstream only calls
/// `negotiate_daemon_auth()` from `auth_client()`, so a server list the client
/// cannot match is inert until credentials are actually demanded.
fn spawn_stub_daemon(banner: &'static str) -> (SocketAddr, thread::JoinHandle<Vec<String>>) {
    let listener =
        TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("bind stub daemon");
    let addr = listener.local_addr().expect("stub daemon address");

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept client");
        stream
            .set_read_timeout(Some(CLIENT_DEADLINE))
            .expect("read timeout");
        let mut writer = stream.try_clone().expect("clone stub stream");
        let mut reader = BufReader::new(stream);

        writer.write_all(banner.as_bytes()).expect("send banner");
        writer.flush().expect("flush banner");

        // The client's greeting, then its module request.
        let mut received = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return received;
            }
            received.push(line.trim_end().to_owned());
        }

        writer
            .write_all(b"@RSYNCD: AUTHREQD AAAAAAAAAAAAAAAAAAAAAA\n")
            .expect("send challenge");
        writer.flush().expect("flush challenge");

        // Anything further is the client's credentials - which upstream never
        // sends on this path. One line is enough: a client that aborted has
        // already closed, so this returns EOF immediately rather than blocking.
        let mut credentials = String::new();
        if reader.read_line(&mut credentials).unwrap_or(0) > 0 {
            received.push(credentials.trim_end().to_owned());
        }
        received
    });

    (addr, handle)
}

/// Runs the shipped client against the stub and returns `(stderr, exit code)`.
fn run_client(addr: SocketAddr) -> (String, Option<i32>) {
    let output = Command::new(oc_rsync_binary())
        .arg("--list-only")
        .arg(format!("rsync://alice@127.0.0.1:{}/module/", addr.port()))
        .env("RSYNC_PASSWORD", "correctpassword")
        .output()
        .expect("run client");

    (
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

/// A server list with no mutual name aborts the client with exit 4, and no
/// credentials ever reach the wire.
#[test]
fn a_server_list_with_no_mutual_digest_aborts_the_client() {
    let (addr, server) = spawn_stub_daemon("@RSYNCD: 32.0 bogus1 bogus2\n");
    let (stderr, code) = run_client(addr);
    let exchanged = server.join().expect("stub daemon thread");

    assert!(
        stderr.contains("Failed to negotiate a daemon auth checksum choice."),
        "client must report upstream's negotiation failure, got:\n{stderr}"
    );
    assert!(
        stderr.contains("Server list: bogus1 bogus2"),
        "client must echo the server's list verbatim, got:\n{stderr}"
    );
    assert!(
        stderr.contains("Client list: sha512 sha256 sha1 md5 md4"),
        "client must echo its own list, got:\n{stderr}"
    );
    assert_eq!(
        code,
        Some(RERR_UNSUPPORTED),
        "client must exit RERR_UNSUPPORTED, stderr was:\n{stderr}"
    );

    // Falling back would have answered the challenge with `alice <hash>`. The
    // exchange must stop at the greeting and the module request.
    assert_eq!(
        exchanged.len(),
        2,
        "no credentials may be sent after a failed negotiation, got: {exchanged:?}"
    );
}

/// The pre-protocol-30 case, where falling back is worst: the substitute there
/// is the seeded `CSUM_MD4_OLD`, a digest upstream emits only when the peer
/// named no list at all.
#[test]
fn a_legacy_server_list_with_no_mutual_digest_aborts_rather_than_seeding_md4() {
    let (addr, server) = spawn_stub_daemon("@RSYNCD: 29 bogus1\n");
    let (stderr, code) = run_client(addr);
    let exchanged = server.join().expect("stub daemon thread");

    assert!(
        stderr.contains("Failed to negotiate a daemon auth checksum choice."),
        "protocol 29 must abort too, got:\n{stderr}"
    );
    assert_eq!(code, Some(RERR_UNSUPPORTED), "stderr was:\n{stderr}");
    assert_eq!(
        exchanged.len(),
        2,
        "no seeded-MD4 response may be sent, got: {exchanged:?}"
    );
}

/// A server that advertises no list at all still authenticates: only the
/// *absent* case gets upstream's protocol-keyed substitute.
#[test]
fn an_absent_server_list_still_falls_back_and_answers_the_challenge() {
    let (addr, server) = spawn_stub_daemon("@RSYNCD: 31.0\n");
    let (_stderr, _code) = run_client(addr);
    let exchanged = server.join().expect("stub daemon thread");

    let credentials = exchanged
        .get(2)
        .unwrap_or_else(|| panic!("client must answer the challenge, exchange was: {exchanged:?}"));
    let (user, response) = credentials
        .split_once(' ')
        .unwrap_or_else(|| panic!("malformed credentials: {credentials:?}"));
    assert_eq!(user, "alice");
    assert_eq!(
        response.len(),
        22,
        "the protocol-31 substitute is md5, whose unpadded base64 is 22 chars"
    );
}
