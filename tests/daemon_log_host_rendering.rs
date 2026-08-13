//! The daemon's peer-host rendering must reproduce upstream's two sentinels.
//!
//! Upstream keeps ONE host string per connection and renders it everywhere the
//! peer is named - the `%h` log escape, `RSYNC_HOST_NAME`, and the
//! `connect from` / `rsync allowed access` / `rsync denied` lines:
//!
//! ```c
//! /* clientserver.c:1524-1526 */
//! addr = client_addr(f_in);
//! host = lp_reverse_lookup(-1) ? client_name(addr) : undetermined_hostname;
//! rprintf(FLOG, "connect from %s (%s)\n", host, addr);
//! /* clientserver.c:770-771 */
//! set_env_str("RSYNC_HOST_NAME", host);
//! set_env_str("RSYNC_HOST_ADDR", addr);
//! ```
//!
//! and that string is never empty: it is `UNDETERMINED` (clientserver.c:126)
//! when reverse lookup is off, and `UNKNOWN` (clientname.c:34) when a lookup
//! was attempted and produced nothing.
//!
//! oc had THREE disagreeing fallbacks for the same missing name - a lowercase
//! `unknown` in the log format, the bare peer IP in the access lines, and an
//! empty string in `RSYNC_HOST_NAME` - so a single session could print
//! `rsync allowed access on module m from 127.0.0.1 (127.0.0.1)` immediately
//! above `h=[unknown]`. This is the class test for that: one rule, rendered
//! identically wherever the peer is named.
//!
//! Skip conditions (test passes with a printed reason):
//! - Loopback TCP is unavailable.
//! - The cross-implementation cell additionally needs a built upstream 3.5.0
//!   binary; without it that cell reports why it did not run.

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A log format naming every field this test reasons about, so one line
/// carries the whole comparison.
const PROBE_FORMAT: &str = "PROBE h=[%h] a=[%a] m=[%m]";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Locates the built upstream 3.5.0 oracle, or `None` when the interop tree has
/// not been fetched and built. The cross-implementation cell degrades to a
/// printed skip rather than a silent pass.
fn upstream_binary() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interop/upstream-src/rsync-3.5.0/rsync");
    path.is_file().then_some(path)
}

/// Claims a loopback port by binding and immediately dropping the listener.
///
/// The daemon binds the same port a moment later. A collision would surface as
/// a failure to serve, which the assertions below report - it cannot turn into
/// a false pass, because every assertion needs a log line the daemon writes.
fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

struct Fixture {
    root: tempfile::TempDir,
    port: u16,
}

impl Fixture {
    fn new(reverse_lookup: &str) -> Option<Self> {
        let root = tempfile::tempdir().expect("temp dir");
        let port = free_port()?;
        fs::create_dir_all(root.path().join("mod")).expect("module dir");
        fs::write(root.path().join("mod/f.txt"), b"hello\n").expect("module file");
        let conf = format!(
            "port = {port}\n\
             use chroot = no\n\
             log file = {log}\n\
             reverse lookup = {reverse_lookup}\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = yes\n\
             \ttransfer logging = yes\n\
             \tlog format = {PROBE_FORMAT}\n",
            log = root.path().join("daemon.log").display(),
            module = root.path().join("mod").display(),
        );
        fs::write(root.path().join("rsyncd.conf"), conf).expect("config");
        Some(Self { root, port })
    }

    fn conf(&self) -> PathBuf {
        self.root.path().join("rsyncd.conf")
    }

    fn log(&self) -> PathBuf {
        self.root.path().join("daemon.log")
    }

    /// Runs one loopback pull against `daemon_bin` and returns the daemon log.
    fn run(&self, daemon_bin: &Path, client_bin: &Path) -> String {
        let mut daemon = spawn_daemon(daemon_bin, &self.conf());
        let dest = self.root.path().join("dest.txt");
        let status = Command::new(client_bin)
            .args([
                "-q",
                &format!("rsync://127.0.0.1:{}/m/f.txt", self.port),
                &dest.display().to_string(),
            ])
            .status()
            .expect("run client");
        let _ = daemon.kill();
        let _ = daemon.wait();
        assert!(status.success(), "pull failed against {daemon_bin:?}");
        fs::read_to_string(self.log()).expect("read daemon log")
    }
}

/// Starts a daemon and waits until its port answers, so the client never races
/// the listener.
fn spawn_daemon(binary: &Path, conf: &Path) -> Child {
    let child = Command::new(binary)
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let port: u16 = fs::read_to_string(conf)
        .expect("read config")
        .lines()
        .find_map(|line| line.strip_prefix("port = ")?.trim().parse().ok())
        .expect("port directive");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// Extracts the single `PROBE ...` line the module's `log format` produced.
fn probe_line(log: &str) -> String {
    log.lines()
        .find(|line| line.contains("PROBE "))
        .map(|line| {
            let start = line.find("PROBE ").expect("PROBE marker");
            line[start..].to_owned()
        })
        .unwrap_or_else(|| panic!("no PROBE line in daemon log:\n{log}"))
}

/// Extracts the `rsync allowed access ... from <host> (<addr>)` line body.
fn access_line(log: &str) -> String {
    log.lines()
        .find(|line| line.contains("rsync allowed access on module"))
        .map(|line| {
            let start = line
                .find("rsync allowed access")
                .expect("allowed-access marker");
            line[start..].to_owned()
        })
        .unwrap_or_else(|| panic!("no allowed-access line in daemon log:\n{log}"))
}

/// With `reverse lookup = no` no name is ever sought, so upstream reports
/// `UNDETERMINED` - not a lowercase `unknown`, and not the address.
///
/// upstream: clientserver.c:126 + :1525.
#[test]
fn reverse_lookup_off_renders_undetermined_everywhere() {
    let Some(fixture) = Fixture::new("no") else {
        println!("SKIP: no loopback port available");
        return;
    };
    let oc = oc_binary();
    let log = fixture.run(&oc, &oc);

    let probe = probe_line(&log);
    assert!(
        probe.contains("h=[UNDETERMINED]"),
        "%h must be upstream's no-lookup sentinel, got: {probe}"
    );
    assert!(
        probe.contains("a=[127.0.0.1]"),
        "%a must be the real peer address, got: {probe}"
    );

    // The access line names the peer with the SAME string. oc used to print the
    // bare IP here while printing `unknown` in the line above.
    let access = access_line(&log);
    assert!(
        access.contains("from UNDETERMINED (127.0.0.1)"),
        "the access line must reuse the host string, got: {access}"
    );
}

/// With reverse lookup ON, a loopback peer resolves on every platform CI runs,
/// so `%h` carries a real name and `%a` the address.
///
/// This is the over-correction guard: a build that always emitted a sentinel
/// would pass the test above and fail here.
///
/// ⚠ It does NOT discriminate on the fix itself: a resolvable peer produced a
/// name before the change too, so this case passes on a pre-fix build. It is
/// kept only to stop the fix being "print a sentinel unconditionally", which
/// nothing else in the file would catch. Measured: with the pre-fix lowercase
/// fallback restored, the other two cases fail and this one still passes.
#[test]
fn reverse_lookup_on_renders_a_resolved_name() {
    let Some(fixture) = Fixture::new("yes") else {
        println!("SKIP: no loopback port available");
        return;
    };
    let oc = oc_binary();
    let log = fixture.run(&oc, &oc);

    let probe = probe_line(&log);
    assert!(
        probe.contains("a=[127.0.0.1]"),
        "%a must be the real peer address, got: {probe}"
    );
    assert!(
        !probe.contains("h=[UNDETERMINED]"),
        "a resolvable peer must not report the no-lookup sentinel: {probe}"
    );
    assert!(
        !probe.contains("h=[unknown]"),
        "the lowercase oc-only fallback must be gone: {probe}"
    );
}

/// CROSS-IMPLEMENTATION: the same config served by the real 3.5.0 binary must
/// produce the same `%h` / `%a` rendering.
///
/// This is what makes the sentinels evidence rather than a guess - the strings
/// `UNDETERMINED` and `UNKNOWN` come from the oracle, not from oc's own source.
#[test]
fn host_rendering_matches_the_real_upstream_daemon() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    let oc = oc_binary();

    for reverse_lookup in ["yes", "no"] {
        let Some(oc_fixture) = Fixture::new(reverse_lookup) else {
            println!("SKIP: no loopback port available");
            return;
        };
        let oc_probe = probe_line(&oc_fixture.run(&oc, &oc));

        let Some(up_fixture) = Fixture::new(reverse_lookup) else {
            println!("SKIP: no loopback port available");
            return;
        };
        let up_probe = probe_line(&up_fixture.run(&upstream, &upstream));

        assert_eq!(
            oc_probe, up_probe,
            "daemon log format diverges from upstream at `reverse lookup = {reverse_lookup}`"
        );
    }
}
