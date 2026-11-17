//! A destination that cannot be stat()ed for any reason but its absence is
//! refused before anything is created, as rsync 3.5.2 refuses it.
//!
//! upstream: main.c:798-803 get_local_name() - `rsyserr(FERROR, errno,
//! "ERROR: cannot stat destination %s", full_fname(dest_path))`, then
//! `exit_cleanup(RERR_FILESELECT)`. The fixture's destination runs through a
//! regular file, so the stat fails with ENOTDIR. oc used to try the mkdir and
//! exit 11 with a `transfer failed: failed to create destination root` line.
//!
//! Oracle cells: the receiver is the client on a pull and the remote-shell
//! server or the daemon on a push; each must print 3.5.2's lines and exit 3.
//! A daemon names the path relative to its module, with ` (in MODULE)` after
//! the closing quote, and the trailing slash stays inside the quotes.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::Command;

use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{
    LshRunnerStub, UpstreamVersion, create_tempdir, oc_rsync_bin, require_upstream_rsync,
};

/// Exit code and stderr, with the program name, source location and version
/// taken out of each line and the fixture root replaced by `ROOT`.
///
/// `rsh` runs the transfer over the remote-shell stub; otherwise `args` name an
/// `rsync://` daemon.
fn normalized(
    binary: &Path,
    root: &Path,
    args: &[String],
    rsh: bool,
) -> (Option<i32>, Vec<String>) {
    let mut command = Command::new(binary);
    command.current_dir(root).arg("-a");
    if rsh {
        let stub = LshRunnerStub::locate().expect("lsh-stub located");
        command
            .arg("-e")
            .arg(stub.path())
            .arg("--rsync-path")
            .arg(binary);
    }
    let out = command.args(args).output().expect("run client");
    let root_text = root.display().to_string();
    let lines = String::from_utf8_lossy(&out.stderr)
        .lines()
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
        .collect();
    (out.status.code(), lines)
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

fn seed(root: &Path) {
    fs::create_dir_all(root.join("src/d")).expect("mkdir src");
    fs::write(root.join("src/d/a"), b"a").expect("seed a");
    fs::write(root.join("src/d/b"), b"b").expect("seed b");
    fs::write(root.join("dstfile"), b"in the way").expect("seed file");
}

fn assert_nothing_created(root: &Path, cell: &str) {
    assert!(
        !root.join("dstfile/x").exists()
            && fs::metadata(root.join("dstfile")).is_ok_and(|m| m.is_file()),
        "nothing was created under the file ({cell})"
    );
}

fn assert_matches_upstream(push: bool) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let tmp = create_tempdir();
    let root = tmp.path();
    seed(root);
    let src = format!("{}/src/d/", root.display());
    let dest = format!("{}/dstfile/x/", root.display());
    let args = if push {
        vec![src, format!("lh:{dest}")]
    } else {
        vec![format!("lh:{src}"), "dstfile/x/".to_owned()]
    };
    let theirs = normalized(upstream.binary(), root, &args, true);
    let ours = normalized(&oc_rsync_bin(), root, &args, true);
    assert_eq!(
        theirs.0,
        Some(3),
        "rsync 3.5.2 exit (push={push}): {theirs:?}"
    );
    assert!(
        theirs.1.first().is_some_and(|line| line
            .starts_with("rsync: [Receiver] ERROR: cannot stat destination \"ROOT/dstfile/x/\": ")),
        "rsync 3.5.2 lines (push={push}): {theirs:?}"
    );
    assert_eq!(ours, theirs, "oc-rsync vs rsync 3.5.2 (push={push})");
    assert_nothing_created(root, &format!("push={push}"));
}

/// A pulling client is the receiver that refuses.
#[test]
fn a_pull_into_a_path_through_a_file_is_refused_like_rsync_3_5_2() {
    assert_matches_upstream(false);
}

/// On a push the remote-shell server is the receiver that refuses.
#[test]
fn a_push_into_a_path_through_a_file_is_refused_like_rsync_3_5_2() {
    assert_matches_upstream(true);
}

/// On a push to a daemon the daemon is the receiver that refuses, naming the
/// destination relative to its module.
#[test]
fn a_daemon_push_into_a_path_through_a_file_is_refused_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let run = |binary: &Path| {
        let tmp = create_tempdir();
        let root = tmp.path();
        seed(root);
        let server = daemon(binary, root);
        let args = vec![
            "src/d/".to_owned(),
            format!("rsync://127.0.0.1:{}/m/dstfile/x/", server.port()),
        ];
        let out = normalized(binary, root, &args, false);
        server.finish().expect("daemon sessions ended");
        assert_nothing_created(root, &binary.display().to_string());
        out
    };
    let theirs = run(upstream.binary());
    assert_eq!(theirs.0, Some(3), "rsync 3.5.2 exit: {theirs:?}");
    assert!(
        theirs.1.first().is_some_and(|line| line.starts_with(
            "rsync: [Receiver] ERROR: cannot stat destination \"dstfile/x/\" (in m): "
        )),
        "rsync 3.5.2 lines: {theirs:?}"
    );
    assert_eq!(run(&oc_rsync_bin()), theirs, "oc-rsync vs rsync 3.5.2");
}
