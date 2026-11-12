//! A pushing client prints the listing an oc server receiver renders.
//!
//! On a push the generator runs on the server. Under `--list-only`
//! `recv_generator()` hands every entry to `list_file_entry()`
//! (upstream: generator.c:1636-1643), whose `rprintf(FINFO, ...)` line
//! (generator.c:1318-1356) `rwrite()` frames as `MSG_INFO` for the client to
//! print (upstream: log.c:355-366). The client forwards `--list-only` because
//! it was given explicitly (upstream: options.c:2923-2924) and moves no data.
//! An oc server receiver collected the entries but never sent them, so the
//! client printed nothing.
//!
//! Every expected listing is the verbatim stdout of rsync 3.5.1 on the same
//! fixture with `TZ=UTC` on both ends. A directory's size is its filesystem
//! `st_size`, which is not portable, so that column is blanked. The symlink's
//! permission column is its `lstat` mode, which `symlink(2)` takes from the
//! umask on macOS but fixes at 0777 on Linux, so it is rebuilt from the
//! fixture's own link (see `for_this_host`). When rsync
//! 3.5.1 is installed the same command also runs with upstream on both ends
//! and with an upstream client against the oc server, and the outputs must be
//! byte-identical.
#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use filetime::{FileTime, set_file_mtime, set_symlink_file_times};
use platform::env::EnvGuard;
use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, UpstreamVersion, create_tempdir,
    oc_rsync_bin, require_binaries, require_upstream_rsync,
};

/// 2020-01-02 03:04:05 UTC.
const FIXTURE_MTIME: i64 = 1_577_934_245;

/// rsync 3.5.1 `-rl --list-only` of [`fixture`]. Under INC_RECURSE each
/// directory heads its own sub-list, so `sub` follows the top-level files.
const LISTING_RL: &str = "\
drwxr-xr-x          4,096 2020/01/02 03:04:05 .
-rw-r--r--              6 2020/01/02 03:04:05 a.txt
lrwxrwxrwx              5 2020/01/02 03:04:05 link -> a.txt
-rw-r--r--              3 2020/01/02 03:04:05 z.txt
drwxr-xr-x          4,096 2020/01/02 03:04:05 sub
-rw-r--r--              6 2020/01/02 03:04:05 sub/b.txt
drwxr-xr-x          4,096 2020/01/02 03:04:05 sub/deep
-rw-r--r--              2 2020/01/02 03:04:05 sub/deep/c.txt
";

/// rsync 3.5.1 `-r --list-only`: without `-l` the server lists the symlink
/// with no ` -> target` (upstream: generator.c:1340-1348 `preserve_links`).
const LISTING_R: &str = "\
drwxr-xr-x          4,096 2020/01/02 03:04:05 .
-rw-r--r--              6 2020/01/02 03:04:05 a.txt
lrwxrwxrwx              5 2020/01/02 03:04:05 link
-rw-r--r--              3 2020/01/02 03:04:05 z.txt
drwxr-xr-x          4,096 2020/01/02 03:04:05 sub
-rw-r--r--              6 2020/01/02 03:04:05 sub/b.txt
drwxr-xr-x          4,096 2020/01/02 03:04:05 sub/deep
-rw-r--r--              2 2020/01/02 03:04:05 sub/deep/c.txt
";

/// Builds `src/{a.txt, link -> a.txt, z.txt, sub/{b.txt, deep/c.txt}}` with
/// fixed modes and mtimes, plus an empty `dst/`.
fn fixture(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub/deep")).expect("create tree");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    for (path, body) in [
        ("a.txt", "hello\n"),
        ("z.txt", "zz\n"),
        ("sub/b.txt", "world\n"),
        ("sub/deep/c.txt", "x\n"),
    ] {
        let file = src.join(path);
        fs::write(&file, body).expect("write file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod file");
    }
    symlink("a.txt", src.join("link")).expect("symlink");
    let mtime = FileTime::from_unix_time(FIXTURE_MTIME, 0);
    set_symlink_file_times(src.join("link"), mtime, mtime).expect("set link mtime");
    // Children before parents so setting a child cannot bump its parent.
    for path in [
        "a.txt",
        "z.txt",
        "sub/b.txt",
        "sub/deep/c.txt",
        "sub/deep",
        "sub",
        "",
    ] {
        let target = src.join(path);
        if target.is_dir() {
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("chmod dir");
        }
        set_file_mtime(target, mtime).expect("set mtime");
    }
}

/// `listing` as it reads on this host: the oracle captured Linux's
/// `lrwxrwxrwx`, and the symlink row carries the fixture link's real mode.
fn for_this_host(listing: &str, root: &Path) -> String {
    let mode = fs::symlink_metadata(root.join("src/link"))
        .expect("lstat link")
        .permissions()
        .mode();
    listing.replace("lrwxrwxrwx", &logging::permstring(mode))
}

/// Blanks the size column of directory rows (see the module docs).
fn normalise(listing: &str) -> String {
    listing
        .lines()
        .map(|line| match line.split_once(' ') {
            Some((perms, rest)) if perms.starts_with('d') => {
                let (_, tail) = rest.trim_start().split_once(' ').expect("size column");
                format!("{perms} <dir-size> {tail}\n")
            }
            _ => format!("{line}\n"),
        })
        .collect()
}

/// Runs `client` with `args`, asserting a clean exit and an empty stderr.
fn run(client: &Path, args: &[String]) -> CliOutput {
    let out = OcRsyncCliRunner::new()
        .binary(client)
        .args(args)
        .env("TZ", "UTC")
        .timeout(Duration::from_secs(30))
        .run()
        .unwrap_or_else(|e| panic!("{args:?}: did not finish: {e}"));
    out.assert_success();
    assert!(
        out.stderr_str().is_empty(),
        "{args:?}: upstream prints nothing on stderr, got: {}",
        out.stderr_str()
    );
    out
}

/// A `--list-only` invocation over the `lsh-stub` remote shell. A push names
/// `src/` and a remote `dst/`; a pull names only the remote `src/`.
fn rsh_args(root: &Path, flags: &str, server: &Path, push: bool) -> Vec<String> {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let mut args = vec![
        flags.to_owned(),
        "--list-only".to_owned(),
        format!("--rsh={}", stub.path().display()),
        format!("--rsync-path={}", server.display()),
    ];
    if push {
        args.push(format!("{}/", root.join("src").display()));
        args.push(format!("localhost:{}/", root.join("dst").display()));
    } else {
        args.push(format!("localhost:{}/", root.join("src").display()));
    }
    args
}

fn assert_listing(out: &CliOutput, expected: &str, what: &str) {
    assert_eq!(
        normalise(&out.stdout_str()),
        normalise(expected),
        "{what}: listing differs from rsync 3.5.1"
    );
}

/// upstream: main.c:725 - `get_local_name()` returns NULL under `list_only`,
/// so the receiver never enters, creates or writes the destination.
fn assert_destination_untouched(root: &Path) {
    let entries: Vec<PathBuf> = fs::read_dir(root.join("dst"))
        .expect("read dst")
        .map(|entry| entry.expect("dst entry").path())
        .collect();
    assert!(entries.is_empty(), "a listing must not write: {entries:?}");
}

#[test]
fn rsh_push_lists_through_the_oc_server() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    // The server renders the times, so its zone must match the oracle's.
    let _tz = EnvGuard::set("TZ", "UTC".as_ref());
    for (flags, expected) in [("-rl", LISTING_RL), ("-r", LISTING_R)] {
        let tmp = create_tempdir();
        fixture(tmp.path());
        let oc = oc_rsync_bin();
        let out = run(&oc, &rsh_args(tmp.path(), flags, &oc, true));
        let expected = for_this_host(expected, tmp.path());
        assert_listing(&out, &expected, &format!("oc -> oc rsh push {flags}"));
        assert_destination_untouched(tmp.path());

        let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
            continue;
        };
        let reference = run(
            upstream.binary(),
            &rsh_args(tmp.path(), flags, upstream.binary(), true),
        );
        let mixed = run(upstream.binary(), &rsh_args(tmp.path(), flags, &oc, true));
        assert_eq!(
            reference.stdout_str(),
            out.stdout_str(),
            "{flags}: oc -> oc against 3.5.1 -> 3.5.1"
        );
        assert_eq!(
            reference.stdout_str(),
            mixed.stdout_str(),
            "{flags}: 3.5.1 -> oc against 3.5.1 -> 3.5.1"
        );
    }
}

#[test]
fn rsh_pull_control_lists_on_the_oc_client() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let _tz = EnvGuard::set("TZ", "UTC".as_ref());
    let tmp = create_tempdir();
    fixture(tmp.path());
    let oc = oc_rsync_bin();
    let out = run(&oc, &rsh_args(tmp.path(), "-rl", &oc, false));
    assert_listing(&out, &for_this_host(LISTING_RL, tmp.path()), "oc pull");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let reference = run(
            upstream.binary(),
            &rsh_args(tmp.path(), "-rl", upstream.binary(), false),
        );
        assert_eq!(
            reference.stdout_str(),
            out.stdout_str(),
            "oc pull against 3.5.1 pull"
        );
    }
}

/// Starts `daemon` serving `root/dst` as module `m`.
fn spawn_daemon(daemon: &Path, root: &Path) -> InetdServer {
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\n[m]\npath = {}\nread only = no\n",
            root.join("dst").display()
        ),
    )
    .expect("write rsyncd.conf");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind daemon listener");
    let command = InetdCommand::discarding_stderr(daemon)
        .expect("open /dev/null")
        .arg("--config")
        .arg(&config);
    InetdServer::start(listener, command).expect("start daemon")
}

fn daemon_args(root: &Path, daemon: &InetdServer) -> Vec<String> {
    vec![
        "-rl".to_owned(),
        "--list-only".to_owned(),
        format!("{}/", root.join("src").display()),
        format!("rsync://127.0.0.1:{}/m/", daemon.port()),
    ]
}

#[test]
fn daemon_push_lists_through_the_oc_daemon() {
    require_binaries!("oc-rsync");
    // The daemon inherits the zone and renders the times.
    let _tz = EnvGuard::set("TZ", "UTC".as_ref());
    let tmp = create_tempdir();
    fixture(tmp.path());
    let oc = oc_rsync_bin();
    let daemon = spawn_daemon(&oc, tmp.path());
    let out = run(&oc, &daemon_args(tmp.path(), &daemon));
    // A `use chroot = no` module munges symlinks by default (rsyncd.conf.5
    // `munge symlinks`), and the receiver lists the target as it stores it,
    // prefix included (upstream: flist.c:1521-1524, generator.c:1345).
    let munged =
        for_this_host(LISTING_RL, tmp.path()).replace("-> a.txt", "-> /rsyncd-munged/a.txt");
    assert_listing(&out, &munged, "oc -> oc daemon push");
    assert_destination_untouched(tmp.path());

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let mixed = run(upstream.binary(), &daemon_args(tmp.path(), &daemon));
    drop(daemon);
    let upstream_daemon = spawn_daemon(upstream.binary(), tmp.path());
    let reference = run(
        upstream.binary(),
        &daemon_args(tmp.path(), &upstream_daemon),
    );
    assert_eq!(
        reference.stdout_str(),
        out.stdout_str(),
        "oc -> oc daemon against 3.5.1"
    );
    assert_eq!(
        reference.stdout_str(),
        mixed.stdout_str(),
        "3.5.1 -> oc daemon against 3.5.1"
    );
}
