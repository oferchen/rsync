//! `--delete-delay` holds every deletion until the transfer is over, under
//! `--max-delete` and `--one-file-system` too, and the cap counts what rsync
//! 3.5.2 counts.
//!
//! upstream:
//! - generator.c:386-390 delete_in_dir() hands a candidate to
//!   remember_delete() whenever `delete_during == 2`; no other option changes
//!   that, and generator.c:372-377 passes over a mount point first.
//! - generator.c:2991-2992 do_delayed_deletions() replays the remembered
//!   names through delete_item() after the last file has landed, so the cap
//!   (delete.c:217-220) is spent in replay order and every `*deleting` row
//!   follows every transfer row.
//! - delete.c:158-163 delete_dir_contents() calls delete_item() on a
//!   subdirectory whatever emptying it returned: at the cap that is one more
//!   `skipped_deletes`, below it the rmdir fails ENOTEMPTY and names the
//!   directory a second time (delete.c:259-263).
//! - generator.c:2996-3001 reports the skipped total once, exit 25.
//!
//! Every row runs with the same binary on both ends - once rsync 3.5.2, once
//! oc-rsync - pushing and pulling over a remote shell and a daemon, comparing
//! exit code, stdout, stderr and the destination tree.

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

const LEGS: &[(Transport, Direction)] = &[
    (Transport::RemoteShell, Direction::Push),
    (Transport::RemoteShell, Direction::Pull),
    (Transport::Daemon, Direction::Push),
    (Transport::Daemon, Direction::Pull),
];

/// One option set and what rsync 3.5.2 does with it.
struct Row {
    flags: &'static [&'static str],
    /// rsync 3.5.2's exit code, asserted so a row cannot pass vacuously. A
    /// client pushing to a daemon exits 0 whatever this says: it has no child
    /// whose status to take (main.c:1158-1163), and the cap is the server's.
    exit: i32,
    /// A line rsync 3.5.2 prints, and how many times.
    line: &'static str,
    times: usize,
}

const fn row(flags: &'static [&'static str], exit: i32, line: &'static str, times: usize) -> Row {
    Row {
        flags,
        exit,
        line,
        times,
    }
}

const PROTECT: &str = "--filter=P keep";
const SKIPPED_9: &str = "Deletions stopped due to --max-delete limit (9 skipped)";
const SKIPPED_8: &str = "Deletions stopped due to --max-delete limit (8 skipped)";
const SKIPPED_7: &str = "Deletions stopped due to --max-delete limit (7 skipped)";
const SKIPPED_6: &str = "Deletions stopped due to --max-delete limit (6 skipped)";
const KEPT_SUB: &str = "cannot delete non-empty directory: xd/sub";

/// The destination holds ten extraneous entries: `x1`, `x2`, `a/x3`, `a/x4`,
/// `b/x5`, and the directory `xd` with `y1`, `sub/`, `sub/y2` and `sub/keep`.
/// A directory that keeps an entry is not counted when it is a candidate
/// itself (`xd`), and is when it is inside one (`xd/sub`).
const TABLE: &[Row] = &[
    // The cap is spent from the end of the run: `xd/sub/y2` is the first name
    // replayed, and a subdirectory left non-empty at the cap is skipped too.
    row(&["--delete-delay", "--max-delete=0"], 25, SKIPPED_9, 1),
    row(&["--delete-delay", "--max-delete=1"], 25, SKIPPED_8, 1),
    row(&["--delete-delay", "--max-delete=3"], 25, SKIPPED_6, 1),
    row(
        &["--delete-delay", "--max-delete=1", "-n"],
        25,
        SKIPPED_8,
        1,
    ),
    row(
        &["--delete-delay", "--max-delete=100"],
        0,
        "*deleting   xd/",
        1,
    ),
    // The same count without the delay: the subdirectory skip is the cap's.
    row(&["--delete-during", "--max-delete=0"], 25, SKIPPED_9, 1),
    row(&["--delete-after", "--max-delete=0"], 25, SKIPPED_9, 1),
    // A protected file keeps `xd/sub`: below the cap its rmdir names it again.
    row(
        &["--delete-delay", "--max-delete=100", PROTECT],
        0,
        KEPT_SUB,
        2,
    ),
    row(
        &["--delete-during", "--max-delete=100", PROTECT],
        0,
        KEPT_SUB,
        2,
    ),
    // At the cap the rmdir is never tried, so it is named once and skipped.
    row(
        &["--delete-delay", "--max-delete=1", PROTECT],
        25,
        KEPT_SUB,
        1,
    ),
    row(
        &["--delete-delay", "--max-delete=1", PROTECT],
        25,
        SKIPPED_7,
        1,
    ),
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
    for dir in ["src/a", "src/b", "dst/a", "dst/b", "dst/xd/sub"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    for file in ["src/a/f1", "src/b/f2", "src/n1", "src/z9"] {
        fs::write(root.join(file), file).expect("seed source file");
    }
    for file in [
        "dst/x1",
        "dst/x2",
        "dst/a/x3",
        "dst/a/x4",
        "dst/b/x5",
        "dst/xd/y1",
        "dst/xd/sub/y2",
        "dst/xd/sub/keep",
    ] {
        fs::write(root.join(file), b"extraneous").expect("seed destination file");
    }
    for dir in ["dst", "dst/a", "dst/b"] {
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

/// Runs `flags` with `binary` on both ends of a fresh fixture.
fn run(binary: &Path, leg: (Transport, Direction), flags: &[&str]) -> Outcome {
    run_pair(binary, binary, leg, flags)
}

/// Runs `flags` with `client` at the near end and `server` at the far end of
/// a fresh fixture.
fn run_pair(
    client: &Path,
    server: &Path,
    (transport, direction): (Transport, Direction),
    flags: &[&str],
) -> Outcome {
    let tmp = create_tempdir();
    let root = tmp.path();
    seed(root);
    let mut command = Command::new(client);
    command.current_dir(root).arg("-rti").args(flags);
    let server = match transport {
        Transport::RemoteShell => {
            let stub = LshRunnerStub::locate().expect("lsh-stub located");
            command
                .arg("-e")
                .arg(stub.path())
                .arg("--rsync-path")
                .arg(server);
            let base = root.display();
            match direction {
                Direction::Push => command.arg("src/").arg(format!("lh:{base}/dst/")),
                Direction::Pull => command.arg(format!("lh:{base}/src/")).arg("dst/"),
            };
            None
        }
        Transport::Daemon => {
            let server = daemon(server, root);
            let url = format!("rsync://127.0.0.1:{}/m", server.port());
            match direction {
                Direction::Push => command.arg("src/").arg(format!("{url}/dst/")),
                Direction::Pull => command.arg(format!("{url}/src/")).arg("dst/"),
            };
            Some(server)
        }
    };
    let out = command.output().expect("run client");
    if let Some(server) = server {
        server.finish().expect("daemon sessions ended");
    }
    (
        out.status.code(),
        normalized(&out.stdout, root),
        normalized(&out.stderr, root),
        tree(root),
    )
}

/// True for a line the delete pass prints: a deletion or a kept directory.
fn is_delete_line(line: &str) -> bool {
    line.starts_with("*deleting ") || line.starts_with("cannot delete ")
}

/// `outcome` with the delete pass's lines moved ahead of the rest of stdout,
/// each group keeping its order.
///
/// A `--delete-during` generator sends its deletions as messages and its
/// itemize rows as data (log.c:866 `MSG_DELETED`), so how the two interleave on
/// the client is up to the I/O buffer, not the option under test.
fn without_interleaving((exit, stdout, stderr, tree): Outcome) -> Outcome {
    let (mut deletes, rows): (Vec<_>, Vec<_>) =
        stdout.into_iter().partition(|line| is_delete_line(line));
    deletes.extend(rows);
    (exit, deletes, stderr, tree)
}

/// Asserts that `stdout` shows every transfer row before the first line of the
/// delete pass, which is what `--delete-delay` is for.
fn assert_deletions_follow_transfers(stdout: &[String], cell: &str) {
    let first_delete = stdout
        .iter()
        .position(|line| is_delete_line(line))
        .unwrap_or_else(|| panic!("rsync 3.5.2 printed no delete line, {cell}: {stdout:?}"));
    let last_transfer = stdout
        .iter()
        .rposition(|line| line.contains("f+++++++++ "))
        .unwrap_or_else(|| panic!("rsync 3.5.2 printed no transfer row, {cell}: {stdout:?}"));
    assert!(
        last_transfer < first_delete,
        "rsync 3.5.2 deletes after the transfer, {cell}: {stdout:?}"
    );
}

#[test]
fn delete_delay_defers_and_caps_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = oc_rsync_bin();
    let mut differing = Vec::new();
    for &leg in LEGS {
        for row in TABLE {
            let cell = format!("{leg:?} {:?}", row.flags);
            let theirs = run(upstream.binary(), leg, row.flags);
            let exit = match leg {
                (Transport::Daemon, Direction::Push) => 0,
                _ => row.exit,
            };
            assert_eq!(theirs.0, Some(exit), "rsync 3.5.2 exit, {cell}: {theirs:?}");
            let printed = theirs.1.iter().chain(&theirs.2);
            assert_eq!(
                printed.filter(|line| *line == row.line).count(),
                row.times,
                "rsync 3.5.2 prints {:?} {} time(s), {cell}: {theirs:?}",
                row.line,
                row.times
            );
            let mut ours = run(&oc, leg, row.flags);
            let theirs = if row.flags.contains(&"--delete-delay") {
                assert_deletions_follow_transfers(&theirs.1, &cell);
                theirs
            } else {
                ours = without_interleaving(ours);
                without_interleaving(theirs)
            };
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

/// A pushed `--max-delete=0` reaches the far end as `--max-delete=-1`
/// (options.c:3067-3072), which every receiver reads as "no deletions"
/// (options.c:2363-2366). Each implementation is put at each end in turn:
/// the outcome is rsync 3.5.2's own, and nothing is deleted.
#[test]
fn a_pushed_max_delete_zero_holds_between_rsync_3_5_2_and_oc() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = oc_rsync_bin();
    let mut differing = Vec::new();
    for transport in [Transport::RemoteShell, Transport::Daemon] {
        let leg = (transport, Direction::Push);
        for flags in [
            &["--delete-delay", "--max-delete=0"],
            &["--delete-after", "--max-delete=0"],
        ] {
            let theirs = run(upstream.binary(), leg, flags);
            for kept in ["x1", "xd/sub/y2"] {
                assert!(
                    theirs.3.iter().any(|path| path == kept),
                    "rsync 3.5.2 keeps {kept}, {leg:?} {flags:?}: {theirs:?}"
                );
            }
            for (name, client, server) in [
                ("3.5.2 client, oc server", upstream.binary(), oc.as_path()),
                ("oc client, 3.5.2 server", oc.as_path(), upstream.binary()),
            ] {
                let mixed = run_pair(client, server, leg, flags);
                if mixed != theirs {
                    differing.push(format!(
                        "{name}, {leg:?} {flags:?}\n  rsync 3.5.2: {theirs:?}\n  mixed:       {mixed:?}"
                    ));
                }
            }
        }
    }
    assert!(
        differing.is_empty(),
        "{} mixed cells differ from rsync 3.5.2:\n{}",
        differing.len(),
        differing.join("\n")
    );
}

/// How a mount namespace can be entered here: unprivileged where the kernel
/// allows it, otherwise through passwordless `sudo`, which then hands the
/// transfer back to the invoking user.
///
/// `None` when neither works. That is a skip only on a run that did not ask
/// for the oracle; one that did (`OC_RSYNC_UPSTREAM_COMPAT=1`) must not count
/// the mount cells as passed without running them.
#[cfg(target_os = "linux")]
fn mount_namespace_entry() -> Option<&'static [&'static str]> {
    const ENTRIES: &[&[&str]] = &[&["unshare", "-rm"], &["sudo", "-n", "unshare", "-m"]];
    let entry = ENTRIES.iter().copied().find(|entry| {
        Command::new(entry[0])
            .args(&entry[1..])
            .arg("true")
            .output()
            .is_ok_and(|out| out.status.success())
    });
    assert!(
        entry.is_some() || !test_support::upstream_compat_enabled(),
        "upstream oracle required (OC_RSYNC_UPSTREAM_COMPAT=1) but no mount namespace \
         can be entered: neither `unshare -rm` nor `sudo -n unshare -m` is permitted"
    );
    entry
}

/// Runs `flags` with `binary` on both ends over a remote shell, inside the
/// mount namespace `entry` opens, where `dst/mnt` and `dst/xd/mnt2` are
/// separate filesystems.
#[cfg(target_os = "linux")]
fn run_across_mounts(
    entry: &[&str],
    binary: &Path,
    direction: Direction,
    flags: &[&str],
) -> Outcome {
    use std::os::unix::fs::MetadataExt;

    let tmp = create_tempdir();
    let root = tmp.path();
    seed(root);
    for dir in ["dst/mnt", "dst/xd/mnt2"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir mount point");
    }
    backdate(&root.join("dst"));
    // The tree is listed inside the namespace: its mounts end with it. Under
    // `unshare -r` the caller already is the namespace's root; under `sudo`
    // the transfer goes back to the user who owns the fixture.
    let script = r#"
        cd "$1"; uid=$2; gid=$3; shift 3
        [ "$(id -u)" = "$uid" ] || { owner="uid=$uid,gid=$gid"; set -- setpriv --reuid="$uid" --regid="$gid" --clear-groups -- "$@"; }
        for dir in dst/mnt dst/xd/mnt2; do
            mount -t tmpfs -o "${owner:-mode=755}" boundary "$dir" && echo inside > "$dir/inside" || exit 99
        done
        "$@" > stdout 2> stderr
        echo $? > status
        find dst | sort > tree
    "#;
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let base = root.display();
    let owner = fs::metadata(root).expect("stat fixture root");
    let (uid, gid) = if entry[0] == "sudo" {
        (owner.uid(), owner.gid())
    } else {
        (0, 0)
    };
    let mut command = Command::new(entry[0]);
    command
        .args(&entry[1..])
        .args(["sh", "-c", script, "sh"])
        .arg(root)
        .arg(uid.to_string())
        .arg(gid.to_string())
        .arg(binary)
        .arg("-rti")
        .args(flags)
        .arg("-e")
        .arg(stub.path())
        .arg("--rsync-path")
        .arg(binary);
    match direction {
        Direction::Push => command.arg("src/").arg(format!("lh:{base}/dst/")),
        Direction::Pull => command.arg(format!("lh:{base}/src/")).arg("dst/"),
    };
    let out = command.output().expect("mount namespace entered");
    assert!(out.status.success(), "mount scenario failed: {out:?}");
    let read = |name: &str| fs::read(root.join(name)).expect("scenario output");
    let status = String::from_utf8_lossy(&read("status")).trim().parse().ok();
    let tree = String::from_utf8_lossy(&read("tree"))
        .lines()
        .map(str::to_owned)
        .collect();
    (
        status,
        normalized(&read("stdout"), root),
        normalized(&read("stderr"), root),
        tree,
    )
}

/// `-x` keeps a mount point and everything above it; `--delete-delay` still
/// waits for the end of the run to remove the rest.
///
/// upstream: generator.c:372-377 leaves `mnt` alone while the directory is
/// scanned; delete.c:144-152 lets `xd/mnt2` pin `xd` when the replay reaches it.
#[cfg(target_os = "linux")]
#[test]
fn delete_delay_defers_across_mount_points_like_rsync_3_5_2() {
    const KEPT_XD: &str = "cannot delete non-empty directory: xd";
    const ROWS: &[&[&str]] = &[
        &["-x", "--delete-delay"],
        &["-x", "--delete-delay", "--max-delete=3"],
    ];
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let Some(entry) = mount_namespace_entry() else {
        eprintln!(
            "SKIPPED {} cells: neither `unshare -rm` nor `sudo -n unshare -m` is \
             permitted here, so no mount point can be made",
            2 * ROWS.len()
        );
        return;
    };
    let oc = oc_rsync_bin();
    let mut differing = Vec::new();
    for direction in [Direction::Push, Direction::Pull] {
        for flags in ROWS {
            let cell = format!("{direction:?} {flags:?}");
            let theirs = run_across_mounts(entry, upstream.binary(), direction, flags);
            assert_eq!(
                theirs.1.iter().filter(|line| *line == KEPT_XD).count(),
                1,
                "rsync 3.5.2 keeps `xd` for its mount point, {cell}: {theirs:?}"
            );
            for kept in ["dst/mnt/inside", "dst/xd/mnt2/inside"] {
                assert!(
                    theirs.3.iter().any(|path| path == kept),
                    "rsync 3.5.2 keeps {kept}, {cell}: {theirs:?}"
                );
            }
            assert_deletions_follow_transfers(&theirs.1, &cell);
            let ours = run_across_mounts(entry, &oc, direction, flags);
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
        2 * ROWS.len(),
        differing.join("\n")
    );
}
