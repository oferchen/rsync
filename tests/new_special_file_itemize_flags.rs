//! A new device or special file is itemized to the peer with the flags rsync
//! 3.5.2 sends.
//!
//! upstream: generator.c:2135-2136 passes `ITEM_LOCAL_CHANGE |
//! ITEM_REPORT_CHANGE` for a node it creates, and `itemize()` adds
//! `ITEM_IS_NEW` for one that did not exist. The receiver left
//! `ITEM_REPORT_CHANGE` out. The printed row is `cS+++++++++` either way, so
//! only the record on the wire shows it: this reads the record out of what
//! the receiving server sends back on a push.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filetime::FileTime;
use test_support::wire_records::{Record, demultiplex, generator_records};
use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(120);

/// `ITEM_LOCAL_CHANGE | ITEM_IS_NEW | ITEM_REPORT_CHANGE` (`rsync.h`).
const NEW_NODE: u16 = (1 << 14) | (1 << 13) | (1 << 1);

/// Pushes `src/`, which holds one FIFO, with rsync 3.5.2 to `server` and
/// returns the records the server sent back.
fn push_a_fifo(
    root: &Path,
    client: &Path,
    server: &Path,
    flags: &[&str],
    tag: &str,
) -> Vec<Record> {
    let dst = root.join(format!("dst-{tag}"));
    fs::create_dir(&dst).expect("create dst");
    let old = FileTime::from_unix_time(1_546_300_800, 0);
    test_support::file_times::set_file_times(&dst, old, old).expect("backdate dst");
    let capture = root.join(format!("sent-{tag}"));
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .env("CAP", &capture)
        .args(flags)
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", server.display()))
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("localhost:{}/", dst.display()));
    match run_deadlined(&mut command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished { status, stderr, .. } => assert!(
            status.success(),
            "{tag}: {}",
            String::from_utf8_lossy(&stderr)
        ),
        Deadlined::Expired { stderr, .. } => {
            panic!("{tag} did not finish: {}", String::from_utf8_lossy(&stderr))
        }
    }
    assert!(
        fs::symlink_metadata(dst.join("fifo")).is_ok(),
        "{tag}: the FIFO was created"
    );
    generator_records(&demultiplex(&fs::read(&capture).expect("captured stream")))
}

#[test]
fn a_new_fifo_is_itemized_to_a_pushing_client_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    let src = root.join("src");
    fs::create_dir(&src).expect("create src");
    let made = Command::new("mkfifo")
        .arg(src.join("fifo"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    // The remote shell runs the server locally and copies everything it
    // sends back into the file named by `CAP`. The copier inherits the
    // client's stderr, so a run is not over until its capture is complete.
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/bash\nshift\nexec \"$@\" > >(tee \"$CAP\")\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");

    for flags in [&["-a"][..], &["-a", "--no-inc-recursive"]] {
        let tag = flags.join("");
        let expected = push_a_fifo(&root, up, up, flags, &format!("up{tag}"));
        assert!(
            expected.iter().any(|(_, iflags, _)| *iflags == NEW_NODE),
            "3.5.2 {flags:?} must itemize the new FIFO: {expected:x?}"
        );
        assert_eq!(
            push_a_fifo(&root, up, &oc, flags, &format!("oc{tag}")),
            expected,
            "{flags:?}: the records the receiving server sent"
        );
    }
}
