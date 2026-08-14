//! On the inetd path the daemon must read its peer from the inherited socket.
//!
//! Under inetd / socket activation, fd 0 IS the connected socket, so the real
//! client address is available and upstream simply asks for it:
//!
//! ```c
//! /* clientserver.c:1559 */
//! start_daemon(STDIN_FILENO, STDIN_FILENO);
//! /* clientname.c:80-83 - am_daemon > 0 skips the env arm entirely */
//! client_sockaddr(fd, &ss, &length);
//! getnameinfo(..., NI_NUMERICHOST);
//! /* clientname.c:41-45 */
//! if (getpeername(fd, (struct sockaddr *) ss, ss_len)) {
//!     rsyserr(FLOG, errno, "getpeername on fd%d failed", fd);
//!     exit_cleanup(RERR_SOCKETIO);
//! }
//! ```
//!
//! oc previously hardcoded `127.0.0.1:0` here too, on the stated premise that
//! "there is no TCP socket to query" - which is false on this path. Every
//! `hosts allow` / `hosts deny` rule was therefore evaluated against a
//! synthetic localhost.
//!
//! HOW this discriminates: an IPv4-loopback test would be useless, because the
//! fabricated value and the true value would coincide. The daemon's socket is
//! therefore peered over IPv6 loopback, so its true peer is `::1` while the
//! fabricated value was `127.0.0.1`. A build that fabricates fails
//! `hosts allow = ::1` and passes `hosts allow = 127.0.0.1`; a build that reads
//! the socket does the opposite. Both directions are asserted, so neither can
//! pass vacuously - and an earlier revision of this file used `127.0.0.2`,
//! which silently SKIPPED on macOS (only `127.0.0.1` is configured there) and
//! therefore proved nothing on half the CI matrix.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the daemon inherits a socket on fd 0).
//! - IPv6 loopback (`::1`) is unavailable.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

/// A local address distinct from `127.0.0.1`, so the fabricated value and the
/// true value cannot coincide. IPv6 loopback is configured by default on both
/// Linux and macOS, unlike `127.0.0.2`.
const PEER_IP: &str = "::1";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Binds a listener on [`PEER_IP`], or returns `None` when IPv6 loopback is
/// unavailable.
fn bind_peer_listener() -> Option<TcpListener> {
    TcpListener::bind((PEER_IP, 0)).ok()
}

fn write_config(root: &Path, allow: &str) -> PathBuf {
    let module = root.join("mod");
    fs::create_dir_all(&module).expect("mkdir module");
    fs::write(module.join("payload.txt"), b"payload\n").expect("write payload");
    let conf = root.join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = false\n\
             [m]\n\
             \x20   path = {}\n\
             \x20   read only = true\n\
             \x20   hosts allow = {allow}\n",
            module.display()
        ),
    )
    .expect("write rsyncd.conf");
    conf
}

/// Spawns the daemon with `stdin` bound to a socket whose peer is [`PEER_IP`],
/// which is what inetd/socket activation hands a server process.
fn spawn_inetd_daemon(conf: &Path, socket: TcpStream) -> Child {
    // `TcpStream` -> `OwnedFd` -> `Stdio`: the child inherits the socket on
    // fd 0 and fd 1 exactly as inetd would hand it over.
    let stdin = Stdio::from(OwnedFd::from(
        socket.try_clone().expect("clone socket for stdin"),
    ));
    let stdout = Stdio::from(OwnedFd::from(socket));
    Command::new(oc_binary())
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdin(stdin)
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn inetd daemon")
}

/// Speaks the opening `@RSYNCD:` exchange and asks for module `m`, returning
/// the daemon's reply to the module request.
fn request_module(peer: TcpStream) -> String {
    peer.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
    let mut writer = peer.try_clone().expect("clone for write");
    let mut reader = BufReader::new(peer);

    let mut greeting = String::new();
    if reader.read_line(&mut greeting).is_err() || greeting.is_empty() {
        return String::new();
    }
    // Echo a compatible version line INCLUDING the digest name list (protocol
    // 32 rejects a bare version), then name the module.
    let _ = writer.write_all(b"@RSYNCD: 32.0 md5 md4\n");
    let _ = writer.write_all(b"m\n");
    let _ = writer.flush();

    let mut reply = String::new();
    let mut line = String::new();
    while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
        reply.push_str(&line);
        if line.starts_with("@ERROR") || line.starts_with("@RSYNCD: OK") || line == "\n" {
            break;
        }
        line.clear();
    }
    reply
}

/// Runs one inetd session against a module whose `hosts allow` names `allow`,
/// returning the daemon's reply. `None` means IPv6 loopback is unavailable.
fn run_session(allow: &str) -> Option<String> {
    let listener = bind_peer_listener()?;
    let addr = listener.local_addr().expect("local addr");
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = write_config(tmp.path(), allow);

    // The daemon holds the CONNECTING end, so its peer is the listener's
    // address - `127.0.0.2` - and the test drives the client side.
    let daemon_side = TcpStream::connect(addr).expect("connect daemon side");
    let accepted = thread::spawn(move || listener.accept().map(|(stream, _)| stream));

    let mut child = spawn_inetd_daemon(&conf, daemon_side);
    let client_side = accepted
        .join()
        .expect("accept thread")
        .expect("accept client side");

    let reply = request_module(client_side);

    let _ = child.kill();
    let _ = child.wait();
    drop(tmp);
    Some(reply)
}

/// THE headline case: a rule naming the REAL peer must admit it. A build that
/// fabricates `127.0.0.1` denies here, because the true peer never matched.
#[test]
fn allow_rule_naming_the_real_peer_admits_it() {
    let Some(reply) = run_session(PEER_IP) else {
        println!("skipped: IPv6 loopback ({PEER_IP}) is unavailable on this host");
        return;
    };
    assert!(
        !reply.contains("access denied"),
        "`hosts allow = {PEER_IP}` must admit a client whose real peer is \
         {PEER_IP}; daemon replied: {reply:?}"
    );
}

/// The control: a rule naming loopback must NOT admit a peer that is not
/// loopback. A build that fabricates `127.0.0.1` admits here - that is the
/// fail-open the fix closes.
#[test]
fn loopback_allow_does_not_admit_a_non_loopback_peer() {
    let Some(reply) = run_session("127.0.0.1") else {
        println!("skipped: IPv6 loopback ({PEER_IP}) is unavailable on this host");
        return;
    };
    assert!(
        reply.contains("access denied"),
        "`hosts allow = 127.0.0.1` must NOT admit a client whose real peer is \
         {PEER_IP}; daemon replied: {reply:?}"
    );
}
