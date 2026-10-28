//! Cross-implementation oracle: `--fake-super` destinations, oc-rsync against
//! upstream rsync 3.5.1 on the same fixtures.
//!
//! # Why this test exists
//!
//! Upstream's generator reads every destination through `x_lstat()`, which
//! folds the `user.rsync.%stat` record into the stat it compares and itemizes
//! against (xattrs.c:1372, generator.c:1745). oc compared the owner-writable
//! placeholder mode instead, so a rerun itemized `.f...p.....` for every
//! 0444, setuid and setgid file, and a record that did not parse was ignored
//! with exit 0 where upstream exits `RERR_FILEIO` (11).
//!
//! # The contract (Rule 9)
//!
//! For each cell, oc's itemize lines, exit code and diagnostic lines equal
//! upstream's. Only what cannot match by construction is masked: the program
//! name and version in the trailer, the source location, and upstream's
//! `received SIGUSR1 (code 19)` lines, which its forked sibling processes
//! print as they are killed and oc's threads have no counterpart for.
//!
//! # Gating
//!
//! Self-skips when `OC_RSYNC_UPSTREAM_COMPAT` is unset, when no upstream
//! rsync 3.5.1 or oc-rsync binary is available, and when the filesystem
//! refuses user xattrs. Once those hold the cells must run and match.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;
use metadata::FAKE_SUPER_XATTR;
use tempfile::tempdir;
use test_support::{UpstreamVersion, require_upstream_rsync, upstream_compat_enabled};

/// What one run printed and how it ended, with the unmatchable parts masked.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    stdout: String,
    stderr: Vec<String>,
    code: Option<i32>,
}

/// The two implementations under comparison, each run from its own tree.
struct Pair {
    upstream: PathBuf,
    oc: PathBuf,
}

fn pair() -> Option<Pair> {
    if !upstream_compat_enabled() {
        return None;
    }
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1)?
        .binary()
        .to_path_buf();
    let oc = test_support::oc_rsync_bin();
    if !oc.is_file() {
        eprintln!("skipping: oc-rsync binary not built at {}", oc.display());
        return None;
    }
    Some(Pair { upstream, oc })
}

/// Masks the trailer's program name, location and version, and drops the
/// forked-process SIGUSR1 lines (see the module docs).
fn normalise(stderr: &[u8], root: &Path) -> Vec<String> {
    let root = root.display().to_string();
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.contains("received SIGUSR1 (code 19)"))
        .map(|line| {
            let line = line.replace(&root, "<root>");
            let line = line.strip_prefix("oc-").unwrap_or(&line).to_owned();
            match (line.find(" at "), line.rfind(" [")) {
                (Some(at), Some(role)) if at < role => {
                    let role_name = line[role + 2..].split('=').next().unwrap_or_default();
                    format!("{} [{role_name}]", &line[..at])
                }
                _ => line,
            }
        })
        .collect()
}

/// Runs `binary` with `args` from `root` and masks the result.
fn run(binary: &Path, root: &Path, args: &[&str]) -> Outcome {
    let output = Command::new(binary)
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn rsync");
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout)
            .replace(&root.display().to_string(), "<root>"),
        stderr: normalise(&output.stderr, root),
        code: output.status.code(),
    }
}

/// A remote shell that runs the server command locally, as upstream's
/// testsuite `lsh` does.
fn local_shell(root: &Path) -> PathBuf {
    let shell = root.join("lsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write lsh");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod lsh");
    shell
}

/// Pull arguments: `binary` is both the client and, via `--rsync-path`, the
/// server, so each implementation is compared against itself end to end.
fn pull_args<'a>(binary: &'a str, shell: &'a str, flags: &'a str, src: &'a str) -> Vec<&'a str> {
    vec![
        flags,
        "--fake-super",
        "-M--fake-super",
        "-e",
        shell,
        "--rsync-path",
        binary,
        src,
        "dst/",
    ]
}

/// Builds the #164 source tree: modes the placeholder cannot carry, plus a
/// plain file, all with a fixed mtime.
fn mode_tree(root: &Path) -> bool {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    if xattr::set(&src, "user.probe", b"1").is_err() {
        eprintln!("skipping: the filesystem rejects user xattrs");
        return false;
    }
    let _ = xattr::remove(&src, "user.probe");
    for (name, mode) in [
        ("ro", 0o444),
        ("su", 0o4755),
        ("sg", 0o2755),
        ("plain", 0o644),
    ] {
        let path = src.join(name);
        fs::write(&path, name).expect("write source");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod source");
        filetime::set_file_mtime(&path, FileTime::from_unix_time(1_577_836_800, 0)).expect("mtime");
    }
    true
}

/// Moves every destination mtime back so the quick check cannot skip.
fn backdate(dst: &Path) {
    for entry in fs::read_dir(dst).expect("read dst") {
        let path = entry.expect("entry").path();
        filetime::set_file_mtime(&path, FileTime::from_unix_time(1_546_300_800, 0)).expect("mtime");
    }
}

/// Runs the #164 sequence (first run, rerun, backdated run) with `driver`
/// and collects every outcome.
fn mode_sequence(root: &Path, driver: &dyn Fn(&Path) -> Outcome) -> Vec<Outcome> {
    let _ = fs::remove_dir_all(root.join("dst"));
    let first = driver(root);
    let rerun = driver(root);
    backdate(&root.join("dst"));
    let backdated = driver(root);
    vec![first, rerun, backdated]
}

/// upstream: generator.c:1745 -> xattrs.c:1372 x_lstat(). A rerun itemizes
/// nothing; a backdated run reports only the time.
#[test]
fn a_fake_super_rerun_itemizes_like_upstream_locally_and_over_a_pull() {
    let Some(pair) = pair() else { return };
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    if !mode_tree(root) {
        return;
    }
    let shell = local_shell(root);
    let shell = shell.to_str().expect("utf-8 path");
    let src_url = format!("localhost:{}/src/", root.display());
    let local_args = ["-ai", "--fake-super", "src/", "dst/"];
    let mut sequences = Vec::new();
    for binary in [&pair.upstream, &pair.oc] {
        let bin = binary.to_str().expect("utf-8 path");
        let pull_args = pull_args(bin, shell, "-ai", &src_url);
        let local = mode_sequence(root, &|root: &Path| run(binary, root, &local_args));
        let pull = mode_sequence(root, &|root: &Path| run(binary, root, &pull_args));
        sequences.push((local, pull));
    }
    let (upstream, oc) = (&sequences[0], &sequences[1]);
    assert!(
        upstream.0[1].stdout.is_empty(),
        "upstream's local rerun itemizes nothing"
    );
    assert_eq!(oc.0, upstream.0, "local -ai --fake-super sequence");
    assert_eq!(oc.1, upstream.1, "pull -ai --fake-super sequence");
}

/// Plants a fresh `src/f` and an older `dst/f`, with `garbage` as the
/// `%stat` of `corrupt`.
fn corrupt_fixture(root: &Path, corrupt: &str) {
    for dir in ["src", "dst"] {
        let _ = fs::remove_dir_all(root.join(dir));
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
    fs::write(root.join("src/f"), b"data").expect("write src");
    fs::write(root.join("dst/f"), b"old").expect("write dst");
    filetime::set_file_mtime(
        root.join("dst/f"),
        FileTime::from_unix_time(1_546_300_800, 0),
    )
    .expect("mtime");
    xattr::set(root.join(corrupt), FAKE_SUPER_XATTR, b"garbage").expect("plant %stat");
}

/// upstream: xattrs.c:1253-1258 - a record that does not parse is printed
/// with its full_fname() and ends the run with RERR_FILEIO (11), tagged with
/// the role that read it: the generator for a destination, the sender for a
/// local source.
#[test]
fn a_corrupt_fake_super_record_fails_like_upstream() {
    let Some(pair) = pair() else { return };
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    if !mode_tree(root) {
        return;
    }
    let shell = local_shell(root);
    let shell = shell.to_str().expect("utf-8 path");
    let src_url = format!("localhost:{}/src/", root.display());
    let cells: [(&str, &str, bool); 3] = [
        ("local copy onto a corrupt destination", "dst/f", false),
        ("pull onto a corrupt destination", "dst/f", true),
        ("local copy of a corrupt source", "src/f", false),
    ];
    for (cell, corrupt, is_pull) in cells {
        let mut outcomes = Vec::new();
        for binary in [&pair.upstream, &pair.oc] {
            corrupt_fixture(root, corrupt);
            let bin = binary.to_str().expect("utf-8 path");
            let outcome = if is_pull {
                run(binary, root, &pull_args(bin, shell, "-a", &src_url))
            } else {
                run(binary, root, &["-a", "--fake-super", "src/", "dst/"])
            };
            outcomes.push(outcome);
        }
        assert_eq!(
            outcomes[0].code,
            Some(11),
            "{cell}: upstream exits RERR_FILEIO"
        );
        assert_eq!(outcomes[1], outcomes[0], "{cell}");
    }
}

/// One failing server-sender phase: how to plant it and what to run.
struct SenderFailure {
    phase: &'static str,
    plant: fn(&Path),
    flags: &'static [&'static str],
}

fn fresh_src_dst(root: &Path) {
    for dir in ["src", "dst"] {
        let _ = fs::remove_dir_all(root.join(dir));
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
}

fn plant_corrupt(path: &Path) {
    fs::write(path, b"z").expect("write corrupt source");
    xattr::set(path, FAKE_SUPER_XATTR, b"garbage").expect("plant %stat");
}

/// The top-level file list carries the corrupt record.
fn plant_flist(root: &Path) {
    fresh_src_dst(root);
    plant_corrupt(&root.join("src/f"));
}

/// The corrupt record sits in the second sub-list, which the sender builds
/// while `count` files of the first are still in flight.
fn plant_sublist(root: &Path, count: usize) {
    fresh_src_dst(root);
    let first = root.join("src/a");
    fs::create_dir_all(&first).expect("create a");
    fs::create_dir_all(root.join("src/z")).expect("create z");
    for i in 0..count {
        fs::write(first.join(format!("f{i}")), i.to_string()).expect("write small");
    }
    fs::write(first.join("zbig"), vec![0x5a; 20_000_000]).expect("write big");
    plant_corrupt(&root.join("src/z/f"));
}

fn plant_early_sublist(root: &Path) {
    plant_sublist(root, 5);
}

fn plant_late_sublist(root: &Path) {
    plant_sublist(root, 3000);
}

/// A 50 MB basis whose block count at `-B 1024` exceeds what a 1 MiB
/// `--max-alloc` lets the sender's receive_sums() allocate.
fn plant_mid_delta(root: &Path) {
    fresh_src_dst(root);
    let basis = vec![b'a'; 50_000_000];
    fs::write(root.join("dst/m-big"), &basis).expect("write basis");
    filetime::set_file_mtime(
        root.join("dst/m-big"),
        FileTime::from_unix_time(1_546_300_800, 0),
    )
    .expect("mtime");
    let mut source = basis;
    source.push(b'x');
    fs::write(root.join("src/m-big"), source).expect("write source");
    for i in 0..3 {
        fs::write(root.join(format!("src/z-s{i}")), i.to_string()).expect("write small");
    }
}

/// Pulls `src/` from a daemon serving it as module `m`, the daemon run in
/// inetd mode on the one connection the client makes to a listener this test
/// already bound, so there is no port race and no readiness poll.
fn daemon_pull(binary: &Path, root: &Path) -> Option<i32> {
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\nfake super = yes\n[m]\npath = {}/src\n",
            root.display()
        ),
    )
    .expect("write rsyncd.conf");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "rsync://127.0.0.1:{}/m/",
        listener.local_addr().expect("addr").port()
    );
    let daemon_binary = binary.to_path_buf();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        // inetd hands the daemon the socket as both stdin and stdout.
        let output = stream.try_clone().expect("clone socket");
        Command::new(daemon_binary)
            .arg("--daemon")
            .arg(format!("--config={}", config.display()))
            .stdin(std::os::fd::OwnedFd::from(stream))
            .stdout(std::os::fd::OwnedFd::from(output))
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn daemon")
    });
    let client = run(binary, root, &["-a", &url, "dst/"]);
    server.join().expect("daemon thread");
    client.code
}

/// upstream: cleanup.c:244-253 exit_cleanup() queues MSG_ERROR_EXIT and then
/// runs io.c:1066-1075 noop_io_until_death(), which returns before draining
/// while `msgs2stderr` is set (options.c:103). Only a socket daemon clears it
/// (clientserver.c:1178), so an rsh server sender never delivers the frame
/// and its client exits 12, while a daemon sender's client exits with the
/// sender's own code.
///
/// Only exit codes are compared: the trailer role name and the daemon's cause
/// line are separate residuals.
#[test]
fn a_failing_server_sender_exits_like_upstream() {
    let Some(pair) = pair() else { return };
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    if !mode_tree(root) {
        return;
    }
    let shell = local_shell(root);
    let shell = shell.to_str().expect("utf-8 path");
    let src_url = format!("localhost:{}/src/", root.display());
    let rsh: [SenderFailure; 3] = [
        SenderFailure {
            phase: "file list",
            plant: plant_flist,
            flags: &["-a", "--fake-super", "-M--fake-super"],
        },
        SenderFailure {
            phase: "early sub-list",
            plant: plant_early_sublist,
            flags: &["-a", "--fake-super", "-M--fake-super"],
        },
        SenderFailure {
            phase: "late sub-list",
            plant: plant_late_sublist,
            flags: &["-a", "--fake-super", "-M--fake-super"],
        },
    ];
    for cell in &rsh {
        let mut codes = Vec::new();
        for binary in [&pair.upstream, &pair.oc] {
            (cell.plant)(root);
            let bin = binary.to_str().expect("utf-8 path");
            let mut args = cell.flags.to_vec();
            args.extend(["-e", shell, "--rsync-path", bin, &src_url, "dst/"]);
            codes.push(run(binary, root, &args).code);
        }
        assert_eq!(codes[0], Some(12), "{}: upstream's client", cell.phase);
        assert_eq!(codes[1], codes[0], "{}", cell.phase);
    }

    // The client's own exit races the remote shell's: it reports either the
    // broken pipe (10) or, once it has reaped the server, the server's
    // RERR_MALLOC (22). upstream: cleanup.c:146-153.
    let mid_delta = ["-a", "--no-whole-file", "-B", "1024", "-M--max-alloc=1M"];
    for binary in [&pair.upstream, &pair.oc] {
        plant_mid_delta(root);
        let bin = binary.to_str().expect("utf-8 path");
        let mut args = mid_delta.to_vec();
        args.extend(["-e", shell, "--rsync-path", bin, &src_url, "dst/"]);
        let code = run(binary, root, &args).code;
        assert!(
            matches!(code, Some(10 | 22)),
            "mid-delta {}: {code:?}",
            binary.display()
        );
    }

    let mut codes = Vec::new();
    for binary in [&pair.upstream, &pair.oc] {
        plant_flist(root);
        codes.push(daemon_pull(binary, root));
    }
    assert_eq!(codes[0], Some(11), "daemon file list: upstream's client");
    assert_eq!(codes[1], codes[0], "daemon file list");
}

/// The ids of the user running the test, read off a directory it created, so
/// a non-root run can represent the placeholders' records.
fn own_ids(root: &Path) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(root).expect("stat root");
    (meta.uid(), meta.gid())
}

/// Plants the special-file sources: placeholders whose `%stat` names a char
/// device, a block device, a FIFO and a socket, beside a real FIFO and a real
/// socket, all with a fixed mtime. Returns the socket's listener, which keeps
/// it bound for the runs.
fn special_tree(root: &Path) -> std::os::unix::net::UnixListener {
    let src = root.join("src");
    let _ = fs::remove_dir_all(&src);
    fs::create_dir_all(&src).expect("create src");
    let (uid, gid) = own_ids(root);
    for (name, record) in [
        ("cdev", "20644 1,3"),
        ("bdev", "60640 8,1"),
        ("pfifo", "10644 0,0"),
        ("psock", "140755 0,0"),
    ] {
        let path = src.join(name);
        fs::write(&path, b"").expect("write placeholder");
        xattr::set(
            &path,
            FAKE_SUPER_XATTR,
            format!("{record} {uid}:{gid}").as_bytes(),
        )
        .expect("plant %stat");
    }
    let status = Command::new("mkfifo")
        .arg(src.join("rfifo"))
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo");
    let listener = std::os::unix::net::UnixListener::bind(src.join("rsock")).expect("bind");
    set_tree_times(&src, 1_577_836_800);
    listener
}

/// Sets every entry's mtime and atime under `dir` without following links.
fn set_tree_times(dir: &Path, secs: i64) {
    let time = FileTime::from_unix_time(secs, 0);
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        filetime::set_symlink_file_times(&path, time, time).expect("set times");
    }
}

/// Each destination entry's real type and `%stat` record, sorted by name.
fn special_dump(dst: &Path) -> Vec<String> {
    let mut rows: Vec<String> = fs::read_dir(dst)
        .expect("read dst")
        .map(|entry| {
            let path = entry.expect("entry").path();
            let kind = fs::symlink_metadata(&path).expect("lstat").file_type();
            let record = xattr::get(&path, FAKE_SUPER_XATTR)
                .ok()
                .flatten()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            let name = path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            format!("{name} {kind:?} {record}")
        })
        .collect();
    rows.sort();
    rows
}

/// Runs the first, rerun, backdated and rdev-changed passes with `driver`,
/// collecting each outcome and the destination it leaves.
fn special_sequence(root: &Path, driver: &dyn Fn(&Path) -> Outcome) -> Vec<(Outcome, Vec<String>)> {
    let _listener = special_tree(root);
    let _ = fs::remove_dir_all(root.join("dst"));
    let pass = |runs: &mut Vec<(Outcome, Vec<String>)>| {
        let outcome = driver(root);
        runs.push((outcome, special_dump(&root.join("dst"))));
    };
    let mut runs = Vec::new();
    pass(&mut runs);
    pass(&mut runs);
    set_tree_times(&root.join("dst"), 1_546_300_800);
    pass(&mut runs);
    let (uid, gid) = own_ids(root);
    let cdev = root.join("src/cdev");
    xattr::set(
        &cdev,
        FAKE_SUPER_XATTR,
        format!("20644 1,5 {uid}:{gid}").as_bytes(),
    )
    .expect("change rdev");
    filetime::set_file_mtime(&cdev, FileTime::from_unix_time(1_577_836_800, 0)).expect("mtime");
    pass(&mut runs);
    runs
}

/// upstream: xattrs.c:1216-1265 get_stat_xattr() folds the `%stat` type,
/// rdev, mode and ids into every x_lstat(), on the source and on the
/// destination alike, so placeholders follow recv_generator()'s device and
/// special branch (generator.c:2031-2100): the same type that passes
/// quick_check_ok() only has its attributes set (`.D..t` when backdated),
/// another rdev is recreated as `cDc`, and the record survives every pass.
#[test]
fn fake_super_special_placeholders_follow_upstream_locally_and_over_a_pull() {
    let Some(pair) = pair() else { return };
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    if !mode_tree(root) {
        return;
    }
    let shell = local_shell(root);
    let shell = shell.to_str().expect("utf-8 path");
    let src_url = format!("localhost:{}/src/", root.display());
    let local_args = ["-ai", "--fake-super", "src/", "dst/"];
    let mut sequences = Vec::new();
    for binary in [&pair.upstream, &pair.oc] {
        let bin = binary.to_str().expect("utf-8 path");
        let pull_args = pull_args(bin, shell, "-ai", &src_url);
        let local = special_sequence(root, &|root: &Path| run(binary, root, &local_args));
        let pull = special_sequence(root, &|root: &Path| run(binary, root, &pull_args));
        sequences.push((local, pull));
    }
    let (upstream, oc) = (&sequences[0], &sequences[1]);
    assert!(
        upstream.0[1].0.stdout.is_empty(),
        "upstream's local rerun itemizes nothing"
    );
    assert!(
        upstream.0[2].0.stdout.contains(".D..t...... bdev"),
        "upstream's backdated run only sets the time: {}",
        upstream.0[2].0.stdout
    );
    assert_eq!(oc.0, upstream.0, "local -ai --fake-super specials");
    assert_eq!(oc.1, upstream.1, "pull -ai --fake-super specials");
}
