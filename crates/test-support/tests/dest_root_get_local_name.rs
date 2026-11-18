//! The network receiver vets its destination operand the way rsync 3.5.2's
//! get_local_name() does, before anything is transferred or created.
//!
//! upstream: main.c:753-870 get_local_name(), in order:
//! - `--mkpath` runs make_path() first; its failure is `mkdir %s failed`,
//!   RERR_FILEIO (main.c:757-773).
//! - A stat failure other than ENOENT is `ERROR: cannot stat destination %s`,
//!   RERR_FILESELECT (main.c:798-803).
//! - More than one entry, or a trailing slash, needs the root made with a
//!   single mkdir, RERR_FILEIO on failure (main.c:808-822).
//! - A lone entry without a trailing slash enters the destination's parent,
//!   and a parent it cannot enter is `change_dir#3 %s failed`,
//!   RERR_FILESELECT (main.c:845-870).
//!
//! A dry run takes every one of these steps too. The table crosses entry count,
//! destination shape and flags; every row runs over a remote shell and a daemon,
//! pushing and pulling, with the same binary on both ends - once rsync 3.5.2 and
//! once oc-rsync - comparing exit code, stderr and the destination tree.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{
    LshRunnerStub, UpstreamVersion, create_tempdir, oc_rsync_bin, require_upstream_rsync,
};

#[derive(Clone, Copy, Debug)]
enum Transport {
    RemoteShell,
    Daemon,
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Push,
    Pull,
}

/// One destination shape, the flags it runs with and what rsync 3.5.2 does.
struct Row {
    /// Number of transferred entries: 1 is a lone file, 2 a directory.
    entries: usize,
    /// The destination below the fixture's `dst/`.
    dest: &'static str,
    flags: &'static [&'static str],
    /// rsync 3.5.2's exit code, asserted so a row cannot pass vacuously.
    exit: i32,
    /// The start of rsync 3.5.2's first stderr line, after normalization.
    first_line: &'static str,
}

const NONE: &[&str] = &[];
const MKPATH: &[&str] = &["--mkpath"];
const DRY: &[&str] = &["-n"];
const MKPATH_DRY: &[&str] = &["--mkpath", "-n"];

const CHANGE_DIR3: &str = "rsync: [Receiver] change_dir#3 ";
const CANNOT_STAT: &str = "rsync: [Receiver] ERROR: cannot stat destination ";
const MKDIR: &str = "rsync: [Receiver] mkdir ";

const fn row(
    entries: usize,
    dest: &'static str,
    flags: &'static [&'static str],
    exit: i32,
    first_line: &'static str,
) -> Row {
    Row {
        entries,
        dest,
        flags,
        exit,
        first_line,
    }
}

/// `nod` is missing, `locked` is a mode-000 directory, `dstfile` a regular file.
const TABLE: &[Row] = &[
    // A lone file whose parent exists is written under the operand name.
    row(1, "renamed", NONE, 0, ""),
    row(1, "nod/x", NONE, 3, CHANGE_DIR3),
    row(1, "nod/x", MKPATH, 0, ""),
    row(1, "nod/x", DRY, 3, CHANGE_DIR3),
    // main.c:856-864 - a dry run does not enter the parent --mkpath would make.
    row(1, "nod/x", MKPATH_DRY, 0, ""),
    row(1, "locked/x", NONE, 3, CANNOT_STAT),
    // make_path() finds `locked` already a directory and makes nothing, so
    // the stat refusal follows.
    row(1, "locked/x", MKPATH, 3, CANNOT_STAT),
    row(1, "locked/x", DRY, 3, CANNOT_STAT),
    row(1, "locked/x", MKPATH_DRY, 3, CANNOT_STAT),
    row(1, "dstfile/x", NONE, 3, CANNOT_STAT),
    row(1, "dstfile/x", MKPATH, 11, MKDIR),
    row(1, "dstfile/x", DRY, 3, CANNOT_STAT),
    row(1, "dstfile/x", MKPATH_DRY, 11, MKDIR),
    row(1, "dstfile/x/", NONE, 3, CANNOT_STAT),
    row(1, "dstfile/x/", MKPATH, 11, MKDIR),
    row(1, "dstfile/x/", DRY, 3, CANNOT_STAT),
    row(1, "dstfile/x/", MKPATH_DRY, 11, MKDIR),
    row(2, "nod/x", NONE, 11, MKDIR),
    row(2, "nod/x", MKPATH, 0, ""),
    row(2, "nod/x", DRY, 0, ""),
    row(2, "nod/x", MKPATH_DRY, 0, ""),
    row(2, "locked/x", NONE, 3, CANNOT_STAT),
    row(2, "locked/x", MKPATH, 11, MKDIR),
    row(2, "locked/x", DRY, 3, CANNOT_STAT),
    row(2, "locked/x", MKPATH_DRY, 11, MKDIR),
    row(2, "dstfile/x", NONE, 3, CANNOT_STAT),
    row(2, "dstfile/x", MKPATH, 11, MKDIR),
    row(2, "dstfile/x", DRY, 3, CANNOT_STAT),
    row(2, "dstfile/x", MKPATH_DRY, 11, MKDIR),
    row(2, "dstfile/x/", NONE, 3, CANNOT_STAT),
    row(2, "dstfile/x/", MKPATH, 11, MKDIR),
    row(2, "dstfile/x/", DRY, 3, CANNOT_STAT),
    row(2, "dstfile/x/", MKPATH_DRY, 11, MKDIR),
];

const LEGS: &[(Transport, Direction)] = &[
    (Transport::RemoteShell, Direction::Push),
    (Transport::RemoteShell, Direction::Pull),
    (Transport::Daemon, Direction::Push),
    (Transport::Daemon, Direction::Pull),
];

fn seed(root: &Path) {
    fs::create_dir_all(root.join("src/d")).expect("mkdir src");
    fs::write(root.join("src/f"), b"one").expect("seed f");
    fs::write(root.join("src/d/a"), b"a").expect("seed a");
    fs::write(root.join("src/d/b"), b"b").expect("seed b");
    fs::create_dir_all(root.join("dst/locked")).expect("mkdir locked");
    fs::write(root.join("dst/dstfile"), b"in the way").expect("seed file");
    fs::set_permissions(root.join("dst/locked"), fs::Permissions::from_mode(0o000)).expect("lock");
}

/// Whether a mode-000 directory still yields to this process, as it does to
/// root: the `locked` rows then do not refuse for either binary.
fn mode_000_is_enforced() -> bool {
    let tmp = create_tempdir();
    seed(tmp.path());
    let enforced = fs::read_dir(tmp.path().join("dst/locked")).is_err();
    unlock(tmp.path());
    enforced
}

/// Whether `row` needs the mode-000 directory to refuse this process.
fn is_locked(row: &Row) -> bool {
    row.dest.starts_with("locked/")
}

fn unlock(root: &Path) {
    let _ = fs::set_permissions(root.join("dst/locked"), fs::Permissions::from_mode(0o755));
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
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, base, out);
            }
        }
    }
    unlock(root);
    let mut out = Vec::new();
    walk(&root.join("dst"), &root.join("dst"), &mut out);
    out.sort();
    out
}

/// Exit code and stderr, with the program name, source location and version
/// taken out of each line and the fixture root replaced by `ROOT`.
///
/// One line is dropped from both sides: the `received SIGUSR1 (code 19)` line
/// rsync 3.5.2's remote sender prints when a pulling receiver's fatal exit
/// signals it. oc's sender does not print it yet (task 548), and it is no part
/// of the receiver's refusal under test.
fn normalized(stderr: &[u8], root: &Path) -> Vec<String> {
    let root_text = root.display().to_string();
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.contains("received SIGUSR1 (code 19)"))
        .map(|line| {
            let line = line
                .strip_prefix("oc-")
                .unwrap_or(line)
                .replace(&root_text, "ROOT");
            match line.split_once(" at ") {
                Some((text, rest)) => {
                    let role = rest
                        .split_once('[')
                        .and_then(|(_, tag)| tag.split_once('='))
                        .map_or("", |(role, _)| role);
                    format!("{text} [{role}]")
                }
                None => line,
            }
        })
        .collect()
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

/// Runs `row` with `binary` on both ends of a fresh fixture.
fn run(
    binary: &Path,
    (transport, direction): (Transport, Direction),
    row: &Row,
) -> (Option<i32>, Vec<String>, Vec<String>) {
    let tmp = create_tempdir();
    let root = tmp.path();
    seed(root);
    let src = if row.entries == 1 { "src/f" } else { "src/d/" };
    let mut command = Command::new(binary);
    command.current_dir(root).arg("-rt").args(row.flags);
    let server = match transport {
        Transport::RemoteShell => {
            let stub = LshRunnerStub::locate().expect("lsh-stub located");
            command
                .arg("-e")
                .arg(stub.path())
                .arg("--rsync-path")
                .arg(binary);
            let base = root.display();
            match direction {
                Direction::Push => command.arg(src).arg(format!("lh:{base}/dst/{}", row.dest)),
                Direction::Pull => command
                    .arg(format!("lh:{base}/{src}"))
                    .arg(format!("dst/{}", row.dest)),
            };
            None
        }
        Transport::Daemon => {
            let server = daemon(binary, root);
            let url = format!("rsync://127.0.0.1:{}/m", server.port());
            match direction {
                Direction::Push => command.arg(src).arg(format!("{url}/dst/{}", row.dest)),
                Direction::Pull => command
                    .arg(format!("{url}/{src}"))
                    .arg(format!("dst/{}", row.dest)),
            };
            Some(server)
        }
    };
    let out = command.output().expect("run client");
    if let Some(server) = server {
        server.finish().expect("daemon sessions ended");
    }
    (out.status.code(), normalized(&out.stderr, root), tree(root))
}

#[test]
fn the_network_receiver_vets_its_destination_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let enforced = mode_000_is_enforced();
    let locked_cells = LEGS.len() * TABLE.iter().filter(|row| is_locked(row)).count();
    if !enforced {
        eprintln!(
            "SKIPPED {locked_cells} of {} cells: mode 000 does not refuse this \
             (privileged) process, so the `locked/` rows cannot discriminate",
            LEGS.len() * TABLE.len()
        );
    }
    let oc = oc_rsync_bin();
    let mut cells = 0;
    let mut locked_run = 0;
    let mut differing = Vec::new();
    for &leg in LEGS {
        for row in TABLE {
            if is_locked(row) {
                if !enforced {
                    continue;
                }
                locked_run += 1;
            }
            cells += 1;
            let cell = format!(
                "{leg:?} entries={} {} {:?}",
                row.entries, row.dest, row.flags
            );
            let theirs = run(upstream.binary(), leg, row);
            assert_eq!(
                theirs.0,
                Some(row.exit),
                "rsync 3.5.2 exit, {cell}: {theirs:?}"
            );
            assert!(
                theirs
                    .1
                    .first()
                    .map_or("", String::as_str)
                    .starts_with(row.first_line),
                "rsync 3.5.2 first line, {cell}: {theirs:?}"
            );
            let ours = run(&oc, leg, row);
            if ours != theirs {
                differing.push(format!(
                    "{cell}\n  rsync 3.5.2: {theirs:?}\n  oc-rsync:    {ours:?}"
                ));
            }
        }
    }
    // An ordinary user must run every mode-000 cell; only the probe above,
    // which found the mode unenforced, may drop them.
    if enforced {
        assert_eq!(
            locked_run, locked_cells,
            "every `locked/` cell must run when mode 000 is enforced"
        );
    }
    assert!(
        differing.is_empty(),
        "{} of {cells} cells differ from rsync 3.5.2:\n{}",
        differing.len(),
        differing.join("\n")
    );
}
