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

/// A server sender reaches the same `file_checksum()` default case, so an
/// oc server launched by an upstream client refuses the run too.
#[test]
fn server_sender_refuses_a_none_file_checksum() {
    let temp = fixture();
    let output = Command::new(oc_rsync_bin())
        .current_dir(temp.path())
        .args(["--server", "--sender", "-logDtprce.iLsfxCIvu"])
        .arg("--checksum-choice=none")
        .args([".", "src/"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run oc-rsync --server");
    assert!(
        refused(&output),
        "server sender must exit 4 with {REFUSAL:?}; got {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// A pull's client is the receiver and never reaches `file_checksum()`, so
/// the remote sender refuses and the client sees the connection close. Every
/// client/server pairing must give the exit code a 3.5.1 client gets from a
/// 3.5.1 server.
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
    let pull = |client: &Path, server: &Path, dest: &str| -> Output {
        Command::new(client)
            .current_dir(temp.path())
            .args(["-ac", "--checksum-choice=none", "-e"])
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
    for (client, server, dest) in [
        (oc.as_path(), upstream.binary(), "oc-up"),
        (upstream.binary(), oc.as_path(), "up-oc"),
    ] {
        let output = pull(client, server, dest);
        assert_eq!(
            output.status.code(),
            baseline.status.code(),
            "client {} / server {}: {}",
            client.display(),
            server.display(),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(!temp.path().join(dest).join("a").exists());
    }
}
