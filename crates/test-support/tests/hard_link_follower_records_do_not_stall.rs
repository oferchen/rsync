//! A receiver reads the sender's echoes of its hard-link follower records as
//! it writes them.
//!
//! upstream: generator.c:554 `itemize()` writes a record for each follower
//! that is linked (hlink.c:224 `maybe_hard_link()`), and the sender echoes
//! every record that asks for no transfer (sender.c:589-609). rsync's
//! generator and receiver are separate processes, so the echoes are read
//! while the records are written. A receiver that wrote a long list's
//! records in one burst before reading any echo left both sides blocked,
//! each on a buffer the other had stopped reading.
//!
//! The fixture has enough followers, each naming a long leader, to overflow
//! the transport several times over: on this layout the burst first stalled
//! between 1,500 and 1,750 groups with the default 208 KiB socket buffers.
//! On Linux each cell also runs with the server's socket buffers at the
//! kernel minimum, set by the `small-buffer-rsh` helper, so it does not rest
//! on those defaults. Only the server's end can be shrunk from outside the
//! client, so one direction stays at the default.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{
    Deadlined, UpstreamVersion, oc_rsync_bin, require_upstream_rsync, run_deadlined,
};

const BUDGET: Duration = Duration::from_secs(120);
const LINKED_GROUPS: usize = 4000;

/// A remote shell that runs the server locally.
const DIRECT_SHELL: &str = "#!/bin/sh\nshift\nexec \"$@\"\n";

fn write_shell(path: &Path, script: &str) {
    fs::write(path, script).expect("write remote shell");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod remote shell");
}

/// One transfer of the fixture: which binary is the client, which the
/// server, and whether the client receives.
struct Cell {
    label: &'static str,
    client: PathBuf,
    server: PathBuf,
    pull: bool,
}

#[test]
fn many_follower_records_do_not_stall_the_transfer() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary().to_path_buf();
    let oc = oc_rsync_bin();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    let src = root.join("src");
    fs::create_dir(&src).expect("create src");
    let padding = "x".repeat(200);
    for group in 0..LINKED_GROUPS {
        let leader = src.join(format!("a{group:05}{padding}"));
        fs::write(&leader, "").expect("write leader");
        fs::hard_link(&leader, src.join(format!("b{group:05}"))).expect("link follower");
    }
    let direct = root.join("rsh");
    write_shell(&direct, DIRECT_SHELL);
    let shells = [
        ("default buffers", direct),
        #[cfg(target_os = "linux")]
        (
            "smallest buffers",
            PathBuf::from(env!("CARGO_BIN_EXE_small-buffer-rsh")),
        ),
    ];

    let cells = [Cell {
        label: "push from 3.5.2",
        client: up.clone(),
        server: oc.clone(),
        pull: false,
    }];
    let mut run = 0;
    for cell in &cells {
        for (buffers, shell) in &shells {
            for inc in [&[][..], &["--no-inc-recursive"]] {
                let what = format!("{}, {buffers}, {inc:?}", cell.label);
                run += 1;
                let dst = root.join(format!("dst-{run}"));
                fs::create_dir(&dst).expect("create dst");
                let remote = |path: &Path| format!("localhost:{}/", path.display());
                let local = |path: &Path| format!("{}/", path.display());
                let (from, to) = if cell.pull {
                    (remote(&src), local(&dst))
                } else {
                    (local(&src), remote(&dst))
                };
                let mut command = Command::new(&cell.client);
                command
                    .current_dir(&root)
                    .arg("-aH")
                    .args(inc)
                    .arg("-e")
                    .arg(shell)
                    .arg(format!("--rsync-path={}", cell.server.display()))
                    .arg(from)
                    .arg(to);
                match run_deadlined(&mut command, BUDGET).expect("spawn rsync") {
                    Deadlined::Finished { status, stderr, .. } => assert!(
                        status.success(),
                        "{what}: {}",
                        String::from_utf8_lossy(&stderr)
                    ),
                    Deadlined::Expired { .. } => panic!("{what}: the transfer stalled"),
                }
                let linked = (0..LINKED_GROUPS)
                    .filter(|group| {
                        fs::metadata(dst.join(format!("b{group:05}")))
                            .is_ok_and(|meta| meta.nlink() == 2)
                    })
                    .count();
                assert_eq!(linked, LINKED_GROUPS, "{what}: linked followers");
                fs::remove_dir_all(&dst).expect("remove dst");
            }
        }
    }
}
