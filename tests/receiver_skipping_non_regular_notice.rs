//! The receiver reports each non-regular entry it will not create with
//! rsync 3.5.2's `skipping non-regular file "NAME"` line.
//!
//! ```c
//! /* generator.c:2150-2155 recv_generator() */
//! if (ftype != FT_REG) {
//!     if (INFO_GTE(NONREG, 1)) { ... rprintf(FINFO, "skipping non-regular file \"%s\"\n", fname); }
//!     goto cleanup;
//! }
//! ```
//!
//! A symlink reaches that line without `--links` (generator.c:1985), a FIFO
//! without `--specials` (generator.c:2072-2074). NONREG is the only word of
//! `info_verbosity[0]` (options.c:257), so the line prints at default
//! verbosity, and `--info=nonreg0` or a daemon `max verbosity` below 0
//! (options.c:547-573) drops it. On a push the generator runs on the server and
//! the line reaches the client as `MSG_INFO`.
//!
//! Three gaps hid it from an oc peer: the receiver skipped these entries in
//! silence, a remote-shell server left NONREG at 0 when no `-v` arrived, and
//! the sender left FIFOs and devices out of the file list without
//! `--specials` / `--devices`, although flist.c:make_file() sends them. The
//! cross pairs pin the server and sender halves separately.
//!
//! Devices are not covered here: a non-root receiver skips every device, so a
//! device cell cannot separate the `--devices` gate from the `am_root` gate.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(120);
const FIFO: &str = "skipping non-regular file \"sub/fifo\"";
const LINK: &str = "skipping non-regular file \"sub/lnk\"";

/// `(client flags, notices the client prints, in file-list order)`.
const CASES: &[(&[&str], &[&str])] = &[
    (&["-r"], &[FIFO, LINK]),
    (&["-rn"], &[FIFO, LINK]),
    (&["-rl"], &[FIFO]),
    (&["-r", "--specials"], &[LINK]),
    // CONTROL: preserving both types reaches no skip.
    (&["-rl", "--specials"], &[]),
    (&["-r", "--info=nonreg0"], &[]),
    (&["-r", "-q"], &[]),
];

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A tree with one regular file, one symlink and one FIFO, plus the local
/// remote shell that runs the server on this host.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical tempdir");
        let sub = root.join("src/sub");
        fs::create_dir_all(&sub).expect("create src");
        fs::write(root.join("src/f"), b"data\n").expect("write file");
        std::os::unix::fs::symlink("../f", sub.join("lnk")).expect("symlink");
        let made = Command::new("mkfifo")
            .arg(sub.join("fifo"))
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo");
        let rsh = root.join("rsh");
        fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        Self { _temp: temp, root }
    }

    fn fresh_dest(&self, tag: &str) -> PathBuf {
        let dst = self.root.join(format!("dst-{tag}"));
        fs::create_dir(&dst).expect("create dst");
        dst
    }
}

/// The `skipping non-regular file` lines on the client's stdout.
fn notices(command: &mut Command, tag: &str) -> Vec<String> {
    match run_deadlined(command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => {
            assert!(
                status.success(),
                "{tag}: {}",
                String::from_utf8_lossy(&stderr)
            );
            String::from_utf8_lossy(&stdout)
                .lines()
                .filter(|line| line.starts_with("skipping non-regular file"))
                .map(str::to_owned)
                .collect()
        }
        Deadlined::Expired { stderr, .. } => {
            panic!("{tag} did not finish: {}", String::from_utf8_lossy(&stderr))
        }
    }
}

/// Pushes `src/` from `client` to a `server` started by the remote shell.
fn push(fixture: &Fixture, client: &Path, server: &Path, flags: &[&str], tag: &str) -> Vec<String> {
    let dst = fixture.fresh_dest(tag);
    let mut command = Command::new(client);
    command
        .args(flags)
        .arg("-e")
        .arg(fixture.root.join("rsh"))
        .arg(format!("--rsync-path={}", server.display()))
        .arg(format!("{}/", fixture.root.join("src").display()))
        .arg(format!("localhost:{}/", dst.display()));
    notices(&mut command, tag)
}

/// Pushes `src/` from `client` to a `daemon` module capped at `max_verbosity`,
/// or returns `None` when no loopback port is available.
fn daemon_push(
    fixture: &Fixture,
    client: &Path,
    daemon: &Path,
    max_verbosity: i32,
    flags: &[&str],
    tag: &str,
) -> Option<Vec<String>> {
    let port = test_support::inetd_server::reserve_port().ok()?;
    let module = fixture.fresh_dest(tag);
    let conf = fixture.root.join(format!("rsyncd-{tag}.conf"));
    fs::write(
        &conf,
        format!(
            "use chroot = no\n\n[m]\n\tpath = {}\n\tread only = no\n\tmax verbosity = {max_verbosity}\n",
            module.display()
        ),
    )
    .expect("config");
    let command = test_support::inetd_server::InetdCommand::discarding_stderr(daemon)
        .expect("open /dev/null")
        .arg(format!("--config={}", conf.display()));
    let server = test_support::inetd_server::InetdServer::start_reserved(port, command)
        .expect("start daemon");
    let mut client_command = Command::new(client);
    client_command
        .args(flags)
        .arg(format!("{}/", fixture.root.join("src").display()))
        .arg(format!("rsync://127.0.0.1:{port}/m/"));
    let found = notices(&mut client_command, tag);
    server.finish().expect("daemon session ends");
    Some(found)
}

fn tag(label: &str, flags: &[&str]) -> String {
    format!("{label}{}", flags.join("").replace('=', ""))
}

#[test]
fn a_pushing_client_prints_the_notice_from_an_oc_server() {
    let fixture = Fixture::new();
    let oc = oc_binary();
    for &(flags, expected) in CASES {
        assert_eq!(
            push(&fixture, &oc, &oc, flags, &tag("oc", flags)),
            expected,
            "oc push {flags:?}"
        );
    }
}

#[test]
fn a_daemon_push_prints_the_notice_unless_max_verbosity_is_negative() {
    let fixture = Fixture::new();
    let oc = oc_binary();
    for &(flags, expected) in CASES {
        let Some(found) = daemon_push(&fixture, &oc, &oc, 1, flags, &tag("d", flags)) else {
            println!("SKIP: no loopback port available");
            return;
        };
        assert_eq!(found, expected, "oc daemon push {flags:?}");
    }
    // upstream: options.c:547-573 - a negative cap leaves NONREG at 0.
    let Some(found) = daemon_push(&fixture, &oc, &oc, -1, &["-r"], "dcap") else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_eq!(found, Vec::<String>::new(), "`max verbosity = -1`");
}

/// CROSS-IMPLEMENTATION: every pairing of oc and 3.5.2 prints what 3.5.2
/// prints against itself. An upstream client against an oc server isolates
/// the receiving server; an oc client against an upstream server isolates
/// the sender's file list.
#[test]
fn every_push_pairing_prints_what_rsync_3_5_2_prints() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = oc_binary();
    let fixture = Fixture::new();
    for &(flags, _) in CASES {
        let expected = push(&fixture, up, up, flags, &tag("uu", flags));
        for (client, server, label) in [(up, oc.as_path(), "uo"), (oc.as_path(), up, "ou")] {
            assert_eq!(
                push(&fixture, client, server, flags, &tag(label, flags)),
                expected,
                "{label} push {flags:?}"
            );
        }
        let Some(upstream_daemon) = daemon_push(&fixture, up, up, 1, flags, &tag("du", flags))
        else {
            println!("SKIP: no loopback port available");
            return;
        };
        let Some(oc_daemon) = daemon_push(&fixture, up, &oc, 1, flags, &tag("dou", flags)) else {
            println!("SKIP: no loopback port available");
            return;
        };
        assert_eq!(oc_daemon, upstream_daemon, "daemon push to oc {flags:?}");
    }
}
