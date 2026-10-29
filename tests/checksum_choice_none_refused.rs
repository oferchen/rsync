//! `--checksum` with a `none` file checksum is refused like upstream 3.5.1.
//!
//! upstream: checksum.c:548-551 `file_checksum()` - `CSUM_NONE` falls to the
//! default case, prints `Invalid checksum-choice for --checksum: none (0)` and
//! exits `RERR_UNSUPPORTED` (4). The file checksum is the second comma
//! component when there is one (checksum.c:192-205), so `none,md5` is accepted
//! and `md5,none` is refused.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

const REFUSAL: &str = "Invalid checksum-choice for --checksum: none (0)";

/// A source tree with regular files: upstream only reaches `file_checksum()`
/// for a regular file, so the refusal needs one.
fn fixture() -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    fs::create_dir_all(temp.path().join("src/sub")).expect("create source tree");
    fs::write(temp.path().join("src/a"), b"alpha").expect("write a");
    fs::write(temp.path().join("src/sub/b"), b"bravo").expect("write b");
    temp
}

fn run(binary: &Path, root: &Path, choice: &str, checksum: bool, dest: &str) -> Output {
    let mut command = Command::new(binary);
    command.current_dir(root).arg("-a");
    if checksum {
        command.arg("-c");
    }
    command
        .arg(format!("--checksum-choice={choice}"))
        .arg("src/")
        .arg(format!("{dest}/"));
    command.output().expect("run rsync")
}

fn refused(output: &Output) -> bool {
    output.status.code() == Some(4) && String::from_utf8_lossy(&output.stderr).contains(REFUSAL)
}

#[test]
fn checksum_with_a_none_file_checksum_exits_4_with_upstream_text() {
    let temp = fixture();
    for choice in ["none", "md5,none", "auto,none"] {
        let output = run(&oc_rsync_bin(), temp.path(), choice, true, "dst");
        assert!(
            refused(&output),
            "-c --checksum-choice={choice} must exit 4 with {REFUSAL:?}; got {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            !temp.path().join("dst/a").exists(),
            "a refused --checksum-choice={choice} must transfer nothing"
        );
    }
}

#[test]
fn a_none_transfer_checksum_or_no_checksum_flag_still_transfers() {
    let temp = fixture();
    for (choice, checksum) in [("none,md5", true), ("none", false)] {
        let dest = format!("dst-{}-{checksum}", choice.replace(',', "_"));
        let output = run(&oc_rsync_bin(), temp.path(), choice, checksum, &dest);
        assert!(
            output.status.success(),
            "--checksum-choice={choice} (checksum={checksum}) must transfer; got {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            fs::read(temp.path().join(&dest).join("sub/b")).unwrap(),
            b"bravo"
        );
    }
}

/// The same matrix against a real 3.5.1, so the expectations above are
/// upstream's behaviour and not a guess.
#[test]
fn upstream_3_5_1_agrees_on_every_cell() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let temp = fixture();
    let cells = [
        ("none", true, true),
        ("md5,none", true, true),
        ("auto,none", true, true),
        ("none,md5", true, false),
        ("none", false, false),
    ];
    for (index, (choice, checksum, expect_refused)) in cells.into_iter().enumerate() {
        let up = run(
            upstream.binary(),
            temp.path(),
            choice,
            checksum,
            &format!("up{index}"),
        );
        let oc = run(
            &oc_rsync_bin(),
            temp.path(),
            choice,
            checksum,
            &format!("oc{index}"),
        );
        assert_eq!(
            refused(&up),
            expect_refused,
            "upstream cell {choice:?} checksum={checksum}: {:?} {}",
            up.status.code(),
            String::from_utf8_lossy(&up.stderr),
        );
        assert_eq!(
            up.status.code(),
            oc.status.code(),
            "exit code for {choice:?} checksum={checksum}: upstream {} / oc {}",
            String::from_utf8_lossy(&up.stderr),
            String::from_utf8_lossy(&oc.stderr),
        );
        assert_eq!(
            refused(&oc),
            expect_refused,
            "oc cell {choice:?} checksum={checksum}"
        );
    }
}

/// A pull's client is the receiver and never reaches `file_checksum()`, so
/// the remote sender refuses: every client/server pairing fails and transfers
/// nothing.
///
/// An oc client from a 3.5.1 server ends with 3.5.1's exit code and text. A
/// 3.5.1 client from an oc server does not yet: a 3.5.1 sender never flushes
/// the MSG_ERROR_EXIT it queues (cleanup.c:250-254, io.c:1073-1075), so its
/// client sees the connection close (12), while an oc sender delivers the 4,
/// and the oc server's message does not reach that client's stderr. An oc
/// client shows the oc server's refusal, which proves the server refused
/// rather than failing some other way.
#[cfg(unix)]
#[test]
fn pull_leaves_the_refusal_to_the_remote_sender() {
    use std::os::unix::fs::PermissionsExt;

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let temp = fixture();
    let rsh = temp.path().join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    // `md5,none` keeps a real transfer sum, so only the file-sum refusal can
    // stop this pull: an oc server that skipped it would complete it.
    let pull = |client: &Path, server: &Path, dest: &str| -> Output {
        Command::new(client)
            .current_dir(temp.path())
            .args(["-ac", "--checksum-choice=md5,none", "-e"])
            .arg(&rsh)
            .arg(format!("--rsync-path={}", server.display()))
            .arg(format!("localhost:{}/src/", temp.path().display()))
            .arg(format!("{dest}/"))
            .output()
            .expect("run pull")
    };
    let oc = oc_rsync_bin();
    let baseline = pull(upstream.binary(), upstream.binary(), "base");
    assert_ne!(
        baseline.status.code(),
        Some(0),
        "upstream must refuse the pull: {}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    for (client, server, dest, like_upstream) in [
        (oc.as_path(), upstream.binary(), "oc-up", true),
        (upstream.binary(), oc.as_path(), "up-oc", false),
        (oc.as_path(), oc.as_path(), "oc-oc", false),
    ] {
        let output = pull(client, server, dest);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_ne!(
            output.status.code(),
            Some(0),
            "client {} / server {} must refuse: {stderr}",
            client.display(),
            server.display(),
        );
        if like_upstream || client == oc.as_path() {
            assert!(stderr.contains(REFUSAL), "{stderr}");
        }
        if like_upstream {
            assert_eq!(output.status.code(), baseline.status.code(), "{stderr}");
        }
        assert!(!temp.path().join(dest).join("a").exists());
    }
}

/// Upstream refuses only once it sums a file (flist.c:1815-1816), so a source
/// with no regular file is copied.
#[test]
fn a_source_without_regular_files_is_copied() {
    let temp = TempDir::new().expect("tempdir");
    fs::create_dir_all(temp.path().join("src/sub")).expect("create source tree");
    let oc = run(&oc_rsync_bin(), temp.path(), "none", true, "dst");
    assert_eq!(
        oc.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&oc.stderr)
    );
    assert!(temp.path().join("dst/sub").is_dir());
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let up = run(upstream.binary(), temp.path(), "none", true, "up");
        assert_eq!(
            up.status.code(),
            oc.status.code(),
            "exit code against 3.5.1"
        );
    }
}

/// A daemon's sender reaches the same default case: a 3.5.1 client pulling
/// from an oc daemon ends as it does pulling from a 3.5.1 daemon.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_daemon_sender_refuses_like_a_3_5_1_daemon() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let temp = fixture();
    let config = temp.path().join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\n[mod]\npath = {}\nread only = yes\n",
            temp.path().join("src").display()
        ),
    )
    .expect("write rsyncd.conf");
    let pull = |port: u16, dest: &str| -> Output {
        Command::new(upstream.binary())
            .current_dir(temp.path())
            .args(["-ac", "--checksum-choice=none"])
            .arg(format!("rsync://127.0.0.1:{port}/mod/"))
            .arg(format!("{dest}/"))
            .output()
            .expect("run pull")
    };

    let (up_daemon, up_port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(upstream.binary())
            .args(["--daemon", "--no-detach", "--address=127.0.0.1"])
            .arg(format!("--config={}", config.display()))
            .arg(format!("--port={port}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })
    .expect("start the 3.5.1 daemon");
    let up_daemon = KillOnDrop(up_daemon);
    let baseline = pull(up_port, "up");
    drop(up_daemon);
    assert_ne!(
        baseline.status.code(),
        Some(0),
        "3.5.1 must refuse: {}",
        String::from_utf8_lossy(&baseline.stderr)
    );

    let oc_daemon = test_support::DaemonProcess::spawn(&config, ["--address=127.0.0.1"])
        .expect("start oc daemon");
    let output = pull(oc_daemon.port(), "oc");
    assert_eq!(
        output.status.code(),
        baseline.status.code(),
        "3.5.1 client from an oc daemon: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !temp.path().join("oc/a").exists(),
        "a refused pull transfers nothing"
    );
}

/// Kills and reaps a spawned daemon.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct KillOnDrop(std::process::Child);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A push's client is the sender, so it refuses before the remote side
/// receives anything, as a 3.5.1 client does.
#[cfg(unix)]
#[test]
fn push_refuses_in_the_local_sender() {
    use std::os::unix::fs::PermissionsExt;

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let temp = fixture();
    let rsh = temp.path().join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    let push = |client: &Path, dest: &str| -> Output {
        Command::new(client)
            .current_dir(temp.path())
            .args(["-ac", "--checksum-choice=none", "-e"])
            .arg(&rsh)
            .arg(format!("--rsync-path={}", upstream.binary().display()))
            .arg("src/")
            .arg(format!("localhost:{}/{dest}/", temp.path().display()))
            .output()
            .expect("run push")
    };
    let baseline = push(upstream.binary(), "up");
    let output = push(&oc_rsync_bin(), "oc");
    assert_eq!(
        output.status.code(),
        baseline.status.code(),
        "3.5.1: {} / oc: {}",
        String::from_utf8_lossy(&baseline.stderr),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        refused(&output),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !temp.path().join("oc/a").exists(),
        "a refused push transfers nothing"
    );
}
