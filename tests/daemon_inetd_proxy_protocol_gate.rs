//! The inetd daemon runs the same `proxy protocol` gate as the listener.
//!
//! upstream: `daemon_main()` hands an inetd socket straight to `start_daemon()`
//! (clientserver.c:1748-1759), and `start_daemon()` is where the gate lives
//! (clientserver.c:1443-1446):
//!
//! ```c
//! if (lp_proxy_protocol()) {
//!         if (!proxy_peer_allowed(f_in) || !read_proxy_protocol_header(f_in))
//!                 return -1;
//! }
//! ```
//!
//! So an inetd daemon behind a trusted proxy reads the PROXY header before its
//! greeting and treats the proxied address as the peer, and a direct peer that
//! is not a trusted proxy is dropped before anything is sent. oc's inetd path
//! used to skip both: it sent the greeting at once, read the header as the
//! client's version line, and never applied `proxy protocol hosts`.
//!
//! The daemon's socket is peered over IPv6 loopback (`::1`) so the trusted and
//! untrusted cases differ only in the `proxy protocol hosts` value, and the
//! module admits only the proxied address `10.9.8.7`, which no local socket
//! can have: `@RSYNCD: OK` is reachable only through a header the daemon read
//! AND believed.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the daemon inherits a socket on fd 0).
//! - IPv6 loopback (`::1`) is unavailable.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use test_support::ReapOnDrop;

const PEER_IP: &str = "::1";
const PROXIED_IP: &str = "10.9.8.7";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn write_config(root: &Path, trusted_proxies: &str) -> PathBuf {
    let module = root.join("mod");
    fs::create_dir_all(&module).expect("mkdir module");
    let conf = root.join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = false\n\
             proxy protocol = true\n\
             proxy protocol hosts = {trusted_proxies}\n\
             [m]\n\
             \x20   path = {}\n\
             \x20   read only = true\n\
             \x20   hosts allow = {PROXIED_IP}\n",
            module.display()
        ),
    )
    .expect("write rsyncd.conf");
    conf
}

/// Runs one inetd session whose client side first sends a PROXY v1 header
/// naming [`PROXIED_IP`], then speaks `@RSYNCD:` if the daemon greets it.
/// Returns everything the daemon wrote, or `None` when `::1` is unavailable.
fn run_session(trusted_proxies: &str) -> Option<String> {
    let listener = TcpListener::bind((PEER_IP, 0)).ok()?;
    let addr = listener.local_addr().expect("local addr");
    let tmp = tempfile::tempdir().expect("tempdir");
    let conf = write_config(tmp.path(), trusted_proxies);

    let daemon_side = TcpStream::connect(addr).expect("connect daemon side");
    let accepted = thread::spawn(move || listener.accept().map(|(stream, _)| stream));
    let stdin = Stdio::from(OwnedFd::from(
        daemon_side.try_clone().expect("clone socket for stdin"),
    ));
    let stdout = Stdio::from(OwnedFd::from(daemon_side));
    let child = ReapOnDrop::new(
        Command::new(oc_binary())
            .arg("--daemon")
            .arg("--no-detach")
            .arg(format!("--config={}", conf.display()))
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn inetd daemon"),
    );
    let peer = accepted
        .join()
        .expect("accept thread")
        .expect("accept client side");
    peer.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");

    let mut writer = peer.try_clone().expect("clone for write");
    let mut reader = BufReader::new(peer);
    // A proxy sends the header before any client byte, and upstream reads it
    // before writing its greeting.
    let _ = writer.write_all(format!("PROXY TCP4 {PROXIED_IP} 10.0.0.1 40000 873\r\n").as_bytes());
    let _ = writer.flush();

    let mut output = String::new();
    let mut greeting = String::new();
    if reader.read_line(&mut greeting).unwrap_or(0) > 0 {
        output.push_str(&greeting);
        let _ = writer.write_all(b"@RSYNCD: 32.0 md5 md4\nm\n");
        let _ = writer.flush();
        let mut line = String::new();
        while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            output.push_str(&line);
            if line.starts_with("@ERROR") || line.starts_with("@RSYNCD: OK") {
                break;
            }
            line.clear();
        }
    } else {
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest);
        output.push_str(&String::from_utf8_lossy(&rest));
    }

    drop(child);
    drop(tmp);
    Some(output)
}

/// A trusted proxy's header is read before the greeting and names the peer the
/// module's `hosts allow` is checked against.
#[test]
fn inetd_daemon_honours_a_trusted_proxy_header() {
    let Some(output) = run_session(PEER_IP) else {
        println!("skipped: IPv6 loopback ({PEER_IP}) is unavailable on this host");
        return;
    };
    assert!(
        output.starts_with("@RSYNCD: ") && output.contains("@RSYNCD: OK\n"),
        "a trusted proxy's header must be consumed and its address admitted by \
         `hosts allow = {PROXIED_IP}`; daemon wrote: {output:?}"
    );
}

/// A direct peer outside `proxy protocol hosts` is dropped before the greeting,
/// so it cannot choose its own address with a forged header.
#[test]
fn inetd_daemon_drops_an_untrusted_proxy_peer() {
    let Some(output) = run_session("192.0.2.1") else {
        println!("skipped: IPv6 loopback ({PEER_IP}) is unavailable on this host");
        return;
    };
    assert!(
        output.is_empty(),
        "an untrusted proxy peer must get nothing, not even a greeting; \
         daemon wrote: {output:?}"
    );
}
