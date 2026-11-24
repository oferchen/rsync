//! `--remove-source-files` removes every source rsync 3.5.2 removes, not only
//! the files whose data was sent.
//!
//! The sender unlinks a source when the receiving side names it in a
//! `MSG_SUCCESS` (upstream: io.c:1838-1852 -> sender.c:404 `successful_send()`).
//! The receiver sends one for each file it committed (receiver.c:1346), and the
//! generator sends one for each entry it settles without asking for data
//! (generator.c:2297-2300 `return_with_success`):
//!
//! - a regular file, symlink, device or special file already in place
//!   (generator.c:2295, :2011, :2101), `--remove-source-files` only;
//! - a regular file found in a `--link-dest`, `--copy-dest` or
//!   `--compare-dest` directory (generator.c:2199), `--remove-source-files`
//!   only;
//! - a device or special file it made (generator.c:2144),
//!   `--remove-source-files` only;
//! - a symlink it made (generator.c:2060), under `--remove-sent-files` too.
//!
//! A symlink or special file found in a `--link-dest` or `--compare-dest`
//! directory is left alone and is NOT confirmed (generator.c:2024-2032,
//! :2114-2122 `goto cleanup`); `--copy-dest` makes it and confirms it. A dry
//! run confirms nothing.
//!
//! Every cell runs once with rsync 3.5.2 on both ends and once with oc-rsync on
//! both ends, over a remote shell and a daemon, pushing and pulling, and
//! compares the exit code, stdout, stderr and the surviving source tree.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use filetime::FileTime;
use test_support::file_times::set_symlink_file_times;
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

/// One option set and the sources rsync 3.5.2 leaves behind under it.
struct Row {
    flags: &'static [&'static str],
    /// The surviving non-directory sources, asserted against rsync 3.5.2 so a
    /// row cannot pass with both binaries removing nothing.
    survivors: &'static [&'static str],
}

/// What the small fixture holds, by how the generator settles each entry:
///
/// - `xfer.txt` is new and `diff.txt` differs: both are transferred;
/// - `same.txt`, `sub/deep.txt`, `link_same`, `fifo_same` are already in place;
/// - `link_new`, `fifo_new` are made by the generator;
/// - `alt.txt`, `link_alt`, `fifo_alt` exist only in the alternate directory.
const ALL: &[&str] = &[
    "alt.txt",
    "diff.txt",
    "fifo_alt",
    "fifo_new",
    "fifo_same",
    "link_alt",
    "link_new",
    "link_same",
    "same.txt",
    "sub/deep.txt",
    "xfer.txt",
];

/// What `--remove-sent-files` leaves when `alt/` is a `--link-dest` or
/// `--compare-dest` directory: everything but the two transferred files and
/// the made symlink.
const SENT_WITH_ALT: &[&str] = &[
    "alt.txt",
    "fifo_alt",
    "fifo_new",
    "fifo_same",
    "link_alt",
    "link_same",
    "same.txt",
    "sub/deep.txt",
];

const TABLE: &[Row] = &[
    Row {
        flags: &["--remove-source-files"],
        survivors: &[],
    },
    // The deprecated spelling keeps what was neither sent nor a made symlink.
    Row {
        flags: &["--remove-sent-files"],
        survivors: &[
            "fifo_alt",
            "fifo_new",
            "fifo_same",
            "link_same",
            "same.txt",
            "sub/deep.txt",
        ],
    },
    Row {
        flags: &["--remove-source-files", "-n"],
        survivors: ALL,
    },
    Row {
        flags: &["--remove-source-files", "--link-dest=../alt"],
        survivors: &["fifo_alt", "link_alt"],
    },
    Row {
        flags: &["--remove-source-files", "--copy-dest=../alt"],
        survivors: &[],
    },
    Row {
        flags: &["--remove-source-files", "--compare-dest=../alt"],
        survivors: &["fifo_alt", "link_alt"],
    },
    // Under the deprecated spelling an alternate-directory match is never
    // confirmed; only `--copy-dest` makes `link_alt`, and a made symlink is.
    Row {
        flags: &["--remove-sent-files", "--link-dest=../alt"],
        survivors: SENT_WITH_ALT,
    },
    Row {
        flags: &["--remove-sent-files", "--copy-dest=../alt"],
        survivors: &[
            "alt.txt",
            "fifo_alt",
            "fifo_new",
            "fifo_same",
            "link_same",
            "same.txt",
            "sub/deep.txt",
        ],
    },
    Row {
        flags: &["--remove-sent-files", "--compare-dest=../alt"],
        survivors: SENT_WITH_ALT,
    },
];

/// 2020-01-02 03:04:05 UTC, given to every fixture entry so a pre-placed copy
/// passes the quick check and an alternate-directory copy matches in full.
const FIXTURE_MTIME: i64 = 1_577_934_245;

fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo {}", path.display());
}

/// Stamps `path` itself (never a symlink's target) with the fixture mtime.
fn stamp(path: &Path) {
    let mtime = FileTime::from_unix_time(FIXTURE_MTIME, 0);
    set_symlink_file_times(path, mtime, mtime)
        .unwrap_or_else(|e| panic!("stamp {}: {e}", path.display()));
}

fn put_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir parent");
    fs::write(path, content).expect("write file");
    stamp(path);
}

fn put_link(path: &Path) {
    symlink("target", path).expect("symlink");
    stamp(path);
}

fn put_fifo(path: &Path) {
    mkfifo(path);
    stamp(path);
}

/// Builds `src/`, `dst/` and `alt/` under `root` as [`ALL`] describes.
fn seed(root: &Path) {
    let (src, dst, alt) = (root.join("src"), root.join("dst"), root.join("alt"));
    for dir in [&src, &dst, &alt] {
        fs::create_dir_all(dir).expect("mkdir fixture dir");
    }
    put_file(&src.join("xfer.txt"), "new\n");
    put_file(&src.join("diff.txt"), "changed-src\n");
    put_file(&dst.join("diff.txt"), "changed-dst-longer\n");
    for name in ["same.txt", "sub/deep.txt"] {
        put_file(&src.join(name), "same\n");
        put_file(&dst.join(name), "same\n");
    }
    put_file(&src.join("alt.txt"), "alt\n");
    put_file(&alt.join("alt.txt"), "alt\n");
    for dir in [&src, &dst] {
        put_link(&dir.join("link_same"));
        put_fifo(&dir.join("fifo_same"));
    }
    put_link(&src.join("link_new"));
    put_fifo(&src.join("fifo_new"));
    for dir in [&src, &alt] {
        put_link(&dir.join("link_alt"));
        put_fifo(&dir.join("fifo_alt"));
    }
}

/// Every non-directory below `dir`, relative and sorted.
fn files_below(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                walk(&path, base, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .expect("below base")
                        .display()
                        .to_string(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Serves `root` as module `m` with `binary`, one daemon session per
/// connection. Symlinks are stored as sent, so a daemon leg settles them
/// exactly as a remote-shell leg does.
fn daemon(binary: &Path, root: &Path) -> InetdServer {
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\nmunge symlinks = no\npid file = {0}/rsyncd.pid\n\
             [m]\npath = {0}\nread only = no\n",
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

/// What one run leaves: exit code, stdout, stderr, surviving sources.
type Outcome = (Option<i32>, String, String, Vec<String>);

/// Moves `root/src/` into `root/dst/` with `client` as the local end and
/// `server` as the far end.
fn transfer(
    client: &Path,
    server: &Path,
    (transport, direction): (Transport, Direction),
    flags: &[&str],
    root: &Path,
) -> Outcome {
    let mut command = Command::new(client);
    command.current_dir(root).arg("-a").args(flags);
    let daemon = match transport {
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
            let daemon = daemon(server, root);
            let url = format!("rsync://127.0.0.1:{}/m", daemon.port());
            match direction {
                Direction::Push => command.arg("src/").arg(format!("{url}/dst/")),
                Direction::Pull => command.arg(format!("{url}/src/")).arg("dst/"),
            };
            Some(daemon)
        }
    };
    let out = command.output().expect("run client");
    if let Some(daemon) = daemon {
        daemon.finish().expect("daemon sessions ended");
    }
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        files_below(&root.join("src")),
    )
}

/// Runs one table cell with `binary` on both ends of a fresh small fixture.
fn run_cell(binary: &Path, leg: (Transport, Direction), row: &Row) -> Outcome {
    let tmp = create_tempdir();
    seed(tmp.path());
    transfer(binary, binary, leg, row.flags, tmp.path())
}

#[test]
fn the_generator_confirms_what_rsync_3_5_2_confirms() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = oc_rsync_bin();
    let mut differing = Vec::new();
    for &leg in LEGS {
        for row in TABLE {
            let cell = format!("{leg:?} {:?}", row.flags);
            let theirs = run_cell(upstream.binary(), leg, row);
            assert_eq!(
                theirs,
                (
                    Some(0),
                    String::new(),
                    String::new(),
                    row.survivors.iter().map(|s| (*s).to_owned()).collect()
                ),
                "rsync 3.5.2, {cell}"
            );
            let ours = run_cell(&oc, leg, row);
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

/// Each spelling reaches the far end as itself and means the same there.
///
/// The client forwards `--remove-source-files` for mode 1 and
/// `--remove-sent-files` for mode 2 (upstream: options.c:3231-3234), and the
/// receiving side, local or remote, applies that mode. With oc on one end and
/// rsync 3.5.2 on the other, in either role, both spellings must leave what
/// rsync 3.5.2 leaves on its own.
#[test]
fn each_spelling_crosses_the_wire_to_and_from_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let (up, oc) = (upstream.binary(), oc_rsync_bin());
    for row in &TABLE[..2] {
        for direction in [Direction::Push, Direction::Pull] {
            let leg = (Transport::RemoteShell, direction);
            for (client, server, ends) in [
                (&*oc, up, "oc client, 3.5.2 server"),
                (up, &*oc, "3.5.2 client, oc server"),
            ] {
                let tmp = create_tempdir();
                seed(tmp.path());
                assert_eq!(
                    transfer(client, server, leg, row.flags, tmp.path()),
                    (
                        Some(0),
                        String::new(),
                        String::new(),
                        row.survivors.iter().map(|s| (*s).to_owned()).collect()
                    ),
                    "{leg:?} {:?}, {ends}",
                    row.flags
                );
            }
        }
    }
}

const DIRS: usize = 150;
const FILES_PER_DIR: usize = 60;
const FILES_PER_SUBDIR: usize = 10;

/// Builds `src/dNNN/{fNN, sub/gNN, link}` for 150 directories - 10,500 regular
/// files, so the file list crosses many incremental sub-lists - with a FIFO in
/// two of them. In the first, a middle and the last directory one file, the
/// symlink and (where there is one) the FIFO are already in place in `dst/`,
/// and one file sits in `alt/`.
fn seed_large(root: &Path) {
    let (src, dst, alt) = (root.join("src"), root.join("dst"), root.join("alt"));
    for d in 1..=DIRS {
        let dir = src.join(format!("d{d:03}"));
        for f in 1..=FILES_PER_DIR {
            put_file(&dir.join(format!("f{f:02}")), &format!("{d}/{f}\n"));
        }
        for g in 1..=FILES_PER_SUBDIR {
            put_file(&dir.join(format!("sub/g{g:02}")), &format!("s{d}/{g}\n"));
        }
        put_link(&dir.join("link"));
    }
    for d in [1, 75, DIRS] {
        let name = format!("d{d:03}");
        put_file(&dst.join(&name).join("f01"), &format!("{d}/1\n"));
        put_file(&dst.join(&name).join("sub/g10"), &format!("s{d}/10\n"));
        put_link(&dst.join(&name).join("link"));
        put_file(&alt.join(&name).join("f05"), &format!("{d}/5\n"));
    }
    for d in [1, DIRS] {
        let name = format!("d{d:03}");
        put_fifo(&src.join(&name).join("fifo"));
        put_fifo(&dst.join(&name).join("fifo"));
    }
    put_fifo(&src.join("d002/fifo"));
}

/// Runs the large move with `client` and `server`. `--info=flist` makes the
/// client print the file-list banner and nothing else, which names the
/// recursion mode in use.
fn run_large(client: &Path, server: &Path, direction: Direction) -> Outcome {
    let tmp = create_tempdir();
    seed_large(tmp.path());
    transfer(
        client,
        server,
        (Transport::RemoteShell, direction),
        &[
            "--remove-source-files",
            "--compare-dest=../alt",
            "--info=flist",
        ],
        tmp.path(),
    )
}

/// A move of more than 10,000 files under incremental recursion empties the
/// source of everything but its directories, as rsync 3.5.2 does: each
/// sub-list's confirmations reach the sender, for the entries that were sent
/// and for those the generator settled, with either binary on either end.
#[test]
fn an_incremental_move_of_ten_thousand_files_removes_what_rsync_3_5_2_removes() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let (up, oc) = (upstream.binary(), oc_rsync_bin());
    let oracle = run_large(up, up, Direction::Pull);
    assert_eq!(
        oracle,
        (
            Some(0),
            "receiving incremental file list\n".to_owned(),
            String::new(),
            Vec::new()
        ),
        "rsync 3.5.2 pull"
    );
    for (client, server, cell) in [
        (&*oc, &*oc, "oc sender -> oc receiver"),
        (&*oc, up, "3.5.2 sender -> oc receiver"),
        (up, &*oc, "oc sender -> 3.5.2 receiver"),
    ] {
        assert_eq!(
            run_large(client, server, Direction::Pull),
            oracle,
            "pull, {cell}"
        );
    }
    let pushed = run_large(up, up, Direction::Push);
    assert_eq!(
        pushed,
        (
            Some(0),
            "sending incremental file list\n".to_owned(),
            String::new(),
            Vec::new()
        ),
        "rsync 3.5.2 push"
    );
    assert_eq!(
        run_large(&oc, &oc, Direction::Push),
        pushed,
        "push, oc sender -> oc receiver"
    );
}
