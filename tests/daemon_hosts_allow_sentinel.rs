//! `hosts allow = UNKNOWN` / `UNDETERMINED` must match, as upstream documents.
//!
//! ```c
//! /* clientname.c:91-94 - the sentinel is a documented affordance, not an
//!    internal placeholder. */
//! /* If anything goes wrong, including the name->addr->name check, then
//!  * we just use "UNKNOWN", so you can use that value in hosts allow
//!  * lines. */
//! ```
//!
//! Upstream hands `allow_access()` the same never-empty host string its log
//! lines print (clientserver.c:769-773), and `match_hostname` refuses only a
//! NULL or empty name (access.c:37-38) - so a sentinel is matched like any
//! other name. oc modelled the host as `Option<&str>` and passed `None`, making
//! both sentinels unmatchable: `hosts allow = UNDETERMINED` refused every peer.
//!
//! TWO defects, and the first alone was INERT:
//!
//! 1. the sentinel never reached the matcher; and
//! 2. `HostnamePattern::matches` compared case-SENSITIVELY against a pattern
//!    lowercased at parse time. That held only because every host until now
//!    arrived pre-lowercased from DNS. Upstream's matcher is `iwildmatch`, the
//!    case-insensitive form (access.c:46), against a list lowercased by
//!    `strlower` (access.c:251) - so `unknown` matches `UNKNOWN` upstream.
//!
//! Fixing only (1) left every row below still failing, which is why the table
//! asserts the OUTCOME of a real connection rather than the shape of the call.
//!
//! Skip conditions (test passes with a printed reason):
//! - Loopback TCP is unavailable.
//! - The cross-implementation cells additionally need a built upstream 3.5.0
//!   binary; without it they report why they did not run.

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream_binary() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interop/upstream-src/rsync-3.5.0/rsync");
    path.is_file().then_some(path)
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// One row of the access-control table.
struct Case {
    /// `hosts allow` value.
    allow: &'static str,
    /// `reverse lookup` value: "no" leaves the host `UNDETERMINED`; "yes"
    /// resolves loopback to a real name on every platform CI runs.
    reverse_lookup: &'static str,
    /// Whether the peer must be admitted.
    admitted: bool,
    /// Why this row is in the table.
    why: &'static str,
}

const CASES: &[Case] = &[
    Case {
        allow: "UNDETERMINED",
        reverse_lookup: "no",
        admitted: true,
        why: "the sentinel upstream documents as usable in a hosts allow line",
    },
    Case {
        allow: "undetermined",
        reverse_lookup: "no",
        admitted: true,
        why: "upstream lowercases the LIST and matches case-insensitively",
    },
    Case {
        allow: "UNDETER*",
        reverse_lookup: "no",
        admitted: true,
        why: "the wildcard arm folds case too, not just the exact arm",
    },
    // CONTROL: the fix must not admit everyone. A non-matching hostname rule
    // still refuses - without this row, "always allow" would pass every row
    // above.
    Case {
        allow: "other.example",
        reverse_lookup: "no",
        admitted: false,
        why: "CONTROL: an unrelated hostname rule must still refuse",
    },
    // CONTROL: address rules are untouched. Without this row, "the hosts allow
    // directive is being ignored entirely" would also produce a refusal above
    // and look like a pass.
    Case {
        allow: "127.0.0.1",
        reverse_lookup: "no",
        admitted: true,
        why: "CONTROL: an address rule still admits, so the directive is live",
    },
    // THE DISCRIMINATOR. When a name IS found, the sentinel must NOT match:
    // a build that matched the sentinel unconditionally would pass every row
    // above and fail only this one.
    Case {
        allow: "UNDETERMINED",
        reverse_lookup: "yes",
        admitted: false,
        why: "a resolved peer is not UNDETERMINED, so the sentinel must not match",
    },
];

struct Fixture {
    root: tempfile::TempDir,
    port: u16,
}

impl Fixture {
    fn new(allow: &str, reverse_lookup: &str) -> Option<Self> {
        let root = tempfile::tempdir().expect("temp dir");
        let port = free_port()?;
        fs::create_dir_all(root.path().join("mod")).expect("module dir");
        fs::write(root.path().join("mod/f.txt"), b"hello\n").expect("module file");
        let conf = format!(
            "port = {port}\n\
             use chroot = no\n\
             log file = {log}\n\
             reverse lookup = {reverse_lookup}\n\
             hosts allow = {allow}\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = yes\n",
            log = root.path().join("daemon.log").display(),
            module = root.path().join("mod").display(),
        );
        fs::write(root.path().join("rsyncd.conf"), conf).expect("config");
        Some(Self { root, port })
    }

    fn conf(&self) -> PathBuf {
        self.root.path().join("rsyncd.conf")
    }

    /// Runs one loopback pull and reports whether the peer was admitted.
    fn admitted(&self, daemon_bin: &Path, client_bin: &Path) -> bool {
        let mut daemon = spawn_daemon(daemon_bin, &self.conf(), self.port);
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
        status.success() && dest.is_file()
    }
}

/// Starts a daemon and waits until its port answers.
///
/// `stdin` is `/dev/null` deliberately: with an inherited terminal or pipe the
/// daemon takes its single-connection path and never listens, and every row
/// then reports "connection refused" - identical output for rows that must
/// differ. That is a broken instrument, not a result.
fn spawn_daemon(binary: &Path, conf: &Path, port: u16) -> Child {
    let child = Command::new(binary)
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn assert_table(binary: &Path, label: &str) {
    for case in CASES {
        let Some(fixture) = Fixture::new(case.allow, case.reverse_lookup) else {
            println!("SKIP: no loopback port available");
            return;
        };
        let admitted = fixture.admitted(binary, binary);
        assert_eq!(
            admitted,
            case.admitted,
            "{label}: `hosts allow = {}` with `reverse lookup = {}` must {} - {}",
            case.allow,
            case.reverse_lookup,
            if case.admitted { "ADMIT" } else { "REFUSE" },
            case.why
        );
    }
}

#[test]
fn oc_matches_the_host_sentinels_in_hosts_allow() {
    assert_table(&oc_binary(), "oc");
}

/// CROSS-IMPLEMENTATION: the expected column is upstream's behaviour, so assert
/// it against upstream rather than trusting the transcription.
#[test]
fn upstream_matches_the_host_sentinels_in_hosts_allow() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    assert_table(&upstream, "upstream 3.5.0");
}
