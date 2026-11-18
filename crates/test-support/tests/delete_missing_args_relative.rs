//! `--delete-missing-args` removes what a missing operand names the way rsync
//! 3.5.2's delete_item() does, whether or not the operand ends in a slash.
//!
//! upstream:
//! - flist.c:2946-2966 send_file_list() - under `--relative` the whole operand
//!   is stat'ed, so `src/nope/` that does not exist is sent as a mode-0 entry
//!   named `src/nope`, exactly like `src/nope`.
//! - generator.c:1786-1792 recv_generator() - that entry reaches
//!   `delete_item(fname, sx.st.st_mode, del_opts)` and nothing else: it is
//!   never itemized or requested, with or without `--dry-run`.
//! - generator.c:1666 - `del_opts` carries `DEL_RECURSE` only for `--delete` or
//!   `--force`, so a populated directory is otherwise kept and named
//!   (delete.c:113-118, delete.c:176-179).
//! - delete.c:217-246 - each removal is capped by `--max-delete`, logged
//!   through log_delete() and counted; syscall.c:848-850 and
//!   syscall.c:1598-1600 only report it under `--dry-run`.
//!
//! Every row runs with the same binary on both ends - once rsync 3.5.2, once
//! oc-rsync - as a local copy and pushing and pulling over a remote shell and
//! a daemon, comparing exit code, stdout, stderr and the destination tree.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{
    LshRunnerStub, UpstreamVersion, create_tempdir, oc_rsync_bin, require_upstream_rsync,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Leg {
    Local,
    ShellPush,
    ShellPull,
    DaemonPush,
    DaemonPull,
}

const LEGS: &[Leg] = &[
    Leg::Local,
    Leg::ShellPush,
    Leg::ShellPull,
    Leg::DaemonPush,
    Leg::DaemonPull,
];

/// One option set, its operands and what rsync 3.5.2 does with them.
struct Row {
    flags: &'static [&'static str],
    /// Source operands, relative to the fixture root.
    operands: &'static [&'static str],
    /// rsync 3.5.2's exit code, asserted so a row cannot pass vacuously. A
    /// client pushing to a daemon exits 0 whatever this says: the cap is the
    /// server's, and the client has no child whose status to take.
    exit: i32,
    /// A line rsync 3.5.2 prints exactly once.
    line: &'static str,
}

const fn row(
    flags: &'static [&'static str],
    operands: &'static [&'static str],
    exit: i32,
    line: &'static str,
) -> Row {
    Row {
        flags,
        operands,
        exit,
        line,
    }
}

const KEPT: &str = "cannot delete non-empty directory: src/nope";
const DELETED_DIR: &str = "*deleting   src/nope/";
const SKIPPED_1: &str = "Deletions stopped due to --max-delete limit (1 skipped)";

/// `dst/src/` holds `nope/` (with `deep/x` and `y`), the empty `nodir/` and the
/// file `nofile`; none of them exists in the source.
const TABLE: &[Row] = &[
    // A populated directory is kept without DEL_RECURSE, slash or no slash.
    row(&[], &["src/nope/"], 0, KEPT),
    row(&[], &["src/nope"], 0, KEPT),
    // An empty directory and a file go, slash or no slash.
    row(&[], &["src/nodir/"], 0, "*deleting   src/nodir/"),
    row(&[], &["src/nofile/"], 0, "*deleting   src/nofile"),
    row(&[], &["src/nofile"], 0, "*deleting   src/nofile"),
    // DEL_RECURSE empties the directory leaf by leaf.
    row(&["--delete"], &["src/nope/"], 0, DELETED_DIR),
    row(&["--force"], &["src/nope"], 0, DELETED_DIR),
    // A dry run reports the same rows and removes nothing.
    row(&["-n", "--delete"], &["src/nope/"], 0, DELETED_DIR),
    row(&["-n"], &["src/nofile"], 0, "*deleting   src/nofile"),
    row(&["-n"], &["src/nope/"], 0, KEPT),
    // The cap is spent inside the directory: `deep/x` and `deep/` go, `y`
    // is refused, and the directory that still holds it is named.
    row(
        &["--delete", "--max-delete=2"],
        &["src/nope/"],
        25,
        SKIPPED_1,
    ),
    row(&["--delete", "--max-delete=2"], &["src/nope/"], 25, KEPT),
];

/// Sets `dir`'s mtime to a fixed past instant, so every destination directory
/// differs from its source and itemizes the same way on every run.
fn backdate(dir: &Path) {
    let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    fs::File::open(dir)
        .and_then(|handle| handle.set_modified(past))
        .expect("backdate directory");
}

fn seed(root: &Path) {
    for dir in ["src", "dst/src/nope/deep", "dst/src/nodir"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    for file in ["dst/src/nope/deep/x", "dst/src/nope/y", "dst/src/nofile"] {
        fs::write(root.join(file), b"stale").expect("seed destination file");
    }
    for dir in ["dst", "dst/src"] {
        backdate(&root.join(dir));
    }
}

/// Every path below `dst/`, sorted.
fn tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read dst").flatten() {
            let path = entry.path();
            out.push(
                path.strip_prefix(base)
                    .expect("below dst")
                    .display()
                    .to_string(),
            );
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(&path, base, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("dst"), &root.join("dst"), &mut out);
    out.sort();
    out
}

/// The lines of `text` with the program name, source location and version
/// taken out and the fixture root replaced by `ROOT`.
fn normalized(text: &[u8], root: &Path) -> Vec<String> {
    let root_text = root.display().to_string();
    String::from_utf8_lossy(text)
        .lines()
        .map(|line| {
            let line = line
                .strip_prefix("oc-")
                .unwrap_or(line)
                .replace(&root_text, "ROOT");
            match line.split_once(" at ") {
                Some((text, rest)) if text.starts_with("rsync error: ") => {
                    let role = rest
                        .split_once('[')
                        .and_then(|(_, tag)| tag.split_once('='))
                        .map_or("", |(role, _)| role);
                    format!("{text} [{role}]")
                }
                _ => line,
            }
        })
        .collect()
}

/// True for a line the deletion prints: a removal or a kept directory.
fn is_delete_line(line: &str) -> bool {
    line.starts_with("*deleting ") || line.starts_with("cannot delete ")
}

/// Serves `root` as module `m` with `binary`, one daemon session per connection.
fn daemon(binary: &Path, root: &Path) -> InetdServer {
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\npid file = {0}/rsyncd.pid\n[m]\npath = {0}\nread only = no\n",
            root.display()
        ),
    )
    .expect("write rsyncd.conf");
    let stderr = fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .expect("open /dev/null");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind daemon listener");
    let command = InetdCommand::new(binary, stderr).arg(format!("--config={}", config.display()));
    InetdServer::start(listener, command).expect("start daemon")
}

/// One run's exit code, stdout, stderr and destination tree.
type Outcome = (Option<i32>, Vec<String>, Vec<String>, Vec<String>);

/// Runs `row` with `binary` on both ends of a fresh fixture.
///
/// The deletion's lines are moved ahead of the rest of stdout, each group
/// keeping its order: a remote generator sends them as messages and its
/// itemize rows as data (log.c:866 `MSG_DELETED`), so how the two interleave
/// on the client is up to the I/O buffer, not the option under test.
fn run(binary: &Path, leg: Leg, row: &Row) -> Outcome {
    let tmp = create_tempdir();
    let root = tmp.path();
    seed(root);
    let base = root.display();
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .args(["-Rri", "--delete-missing-args"])
        .args(row.flags);
    if matches!(leg, Leg::ShellPush | Leg::ShellPull) {
        let stub = LshRunnerStub::locate().expect("lsh-stub located");
        command
            .arg("-e")
            .arg(stub.path())
            .arg("--rsync-path")
            .arg(binary);
    }
    let server = matches!(leg, Leg::DaemonPush | Leg::DaemonPull).then(|| daemon(binary, root));
    let url = server
        .as_ref()
        .map(|server| format!("rsync://127.0.0.1:{}/m", server.port()));
    match (leg, &url) {
        (Leg::Local, _) => command.args(row.operands).arg("dst/"),
        (Leg::ShellPush, _) => command.args(row.operands).arg(format!("lh:{base}/dst/")),
        (Leg::ShellPull, _) => command
            .args(
                row.operands
                    .iter()
                    .map(|name| format!("lh:{base}/./{name}")),
            )
            .arg("dst/"),
        (Leg::DaemonPush, Some(url)) => command.args(row.operands).arg(format!("{url}/dst/")),
        (Leg::DaemonPull, Some(url)) => command
            .args(row.operands.iter().map(|name| format!("{url}/{name}")))
            .arg("dst/"),
        (Leg::DaemonPush | Leg::DaemonPull, None) => unreachable!("a daemon leg has a daemon"),
    };
    let out = command.output().expect("run client");
    if let Some(server) = server {
        server.finish().expect("daemon sessions ended");
    }
    let (mut stdout, rows): (Vec<_>, Vec<_>) = normalized(&out.stdout, root)
        .into_iter()
        .partition(|line| is_delete_line(line));
    stdout.extend(rows);
    (
        out.status.code(),
        stdout,
        normalized(&out.stderr, root),
        tree(root),
    )
}

#[test]
fn delete_missing_args_deletes_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = oc_rsync_bin();
    let mut differing = Vec::new();
    for &leg in LEGS {
        for row in TABLE {
            let cell = format!("{leg:?} {:?} {:?}", row.flags, row.operands);
            let theirs = run(upstream.binary(), leg, row);
            let exit = if leg == Leg::DaemonPush { 0 } else { row.exit };
            assert_eq!(theirs.0, Some(exit), "rsync 3.5.2 exit, {cell}: {theirs:?}");
            let printed = theirs.1.iter().chain(&theirs.2);
            assert_eq!(
                printed.filter(|line| *line == row.line).count(),
                1,
                "rsync 3.5.2 prints {:?} once, {cell}: {theirs:?}",
                row.line
            );
            let ours = run(&oc, leg, row);
            if ours != theirs {
                differing.push(format!(
                    "{cell}\n  rsync 3.5.2: {theirs:?}\n  oc-rsync:    {ours:?}"
                ));
            }
        }
    }
    assert!(
        differing.is_empty(),
        "{} of {} cells differ from rsync 3.5.2:\n{}",
        differing.len(),
        LEGS.len() * TABLE.len(),
        differing.join("\n")
    );
}
