//! Cross-implementation oracle for `--write-batch` iflags fidelity of
//! created directories, symlinks, and special files: an upstream rsync
//! `--read-batch` replay of an oc-produced batch must itemize each created
//! non-regular entry (`cd`/`cL`/`cS`) and count it in `--stats`.
//!
//! `#[ignore]`d because it requires a locally built upstream rsync 3.5.0
//! (`target/interop/upstream-src/rsync-3.5.0/rsync`), absent on a bare
//! checkout; build it with `bash tools/ci/run_interop.sh` or by hand (see
//! `write_batch_upstream_read_batch_itemize.rs`). Run with `--ignored` once
//! built. A missing oracle binary panics loudly rather than silently passing.
//!
//! The always-run companion `write_batch_nonfile_iflags.rs` pins the same
//! defect at the byte level without upstream; this test additionally confirms
//! the itemize rows and `--stats` created breakdown upstream actually prints.
//!
//! # Upstream Reference
//!
//! - `generator.c:1480-1482`/`:1605-1610`/`:1679-1682 itemize()`,
//!   `receiver.c:742-802` (`stats.created_{dirs,symlinks,specials}`),
//!   `receiver.c:575 no_batched_update()` (a missing special entry aborts
//!   the replay with exit 23).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;
use test_support::{Deadlined, run_deadlined};

/// Wall-clock budget for the upstream `--read-batch` subprocess.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Locally built upstream rsync 3.5.0, source-tree layout.
fn upstream_3_5_0() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/interop/upstream-src/rsync-3.5.0/rsync"
    ))
}

/// Panics unless `path` is a genuine upstream rsync 3.5.0 binary.
fn require_upstream(path: &Path) {
    if !path.exists() {
        panic!(
            "upstream rsync 3.5.0 required for this oracle test, but {} is absent. \
             Build it with `bash tools/ci/run_interop.sh` or by hand. This test is \
             #[ignore]d for that reason - run with --ignored once the binary exists.",
            path.display()
        );
    }
    let Ok(output) = Command::new(path).arg("--version").output() else {
        panic!(
            "{} exists but `--version` failed to execute",
            path.display()
        );
    };
    let banner = String::from_utf8_lossy(&output.stdout);
    let first = banner.lines().next().unwrap_or_default();
    if !(first.starts_with("rsync  version 3.5.0") && first.contains("protocol version")) {
        panic!("{} is not upstream rsync 3.5.0: {first:?}", path.display());
    }
}

#[test]
#[ignore = "requires a locally built upstream rsync 3.5.0; run with --ignored"]
fn upstream_read_batch_counts_created_dirs_symlinks_and_specials() {
    let upstream = upstream_3_5_0();
    require_upstream(&upstream);
    let oc_rsync = test_support::oc_rsync_bin();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest_write = temp.path().join("dst_write");
    let dest_read = temp.path().join("dst_read");
    let batch_path = temp.path().join("oc.batch");

    fs::create_dir_all(source.join("d1/d2")).expect("create nested source dirs");
    fs::write(source.join("d1/f2.txt"), b"bb\n").expect("write nested file");
    symlink("d1", source.join("link1")).expect("create source symlink");
    let status = Command::new("mkfifo")
        .arg(source.join("afifo"))
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed");
    fs::create_dir_all(&dest_write).expect("create write-side dest");
    fs::create_dir_all(&dest_read).expect("create read-side dest");

    let mut src_arg = source.clone().into_os_string();
    src_arg.push("/");
    let mut write_cmd = Command::new(&oc_rsync);
    write_cmd
        .arg("-a")
        .arg("-D")
        .arg(format!("--write-batch={}", batch_path.display()))
        .arg(&src_arg)
        .arg(&dest_write);
    let Deadlined::Finished { status, stderr, .. } =
        run_deadlined(&mut write_cmd, UPSTREAM_TIMEOUT).expect("spawn oc-rsync --write-batch")
    else {
        panic!("oc-rsync --write-batch timed out after {UPSTREAM_TIMEOUT:?}");
    };
    assert!(
        status.success(),
        "oc-rsync --write-batch failed: {}",
        String::from_utf8_lossy(&stderr)
    );

    let mut read_cmd = Command::new(&upstream);
    read_cmd
        .arg("-aviD")
        .arg(format!("--read-batch={}", batch_path.display()))
        .arg("--stats")
        .arg(format!("{}/", dest_read.display()));
    let Deadlined::Finished {
        status,
        stdout,
        stderr,
    } = run_deadlined(&mut read_cmd, UPSTREAM_TIMEOUT).expect("spawn upstream --read-batch")
    else {
        panic!("upstream --read-batch timed out after {UPSTREAM_TIMEOUT:?}");
    };
    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        status.success(),
        "upstream --read-batch of an oc batch must not abort (a missing special entry \
         triggers exit 23 via receiver.c:575 no_batched_update); stdout:\n{stdout}\n\
         stderr:\n{}",
        String::from_utf8_lossy(&stderr)
    );

    for row in [
        "cd+++++++++ d1/",
        "cd+++++++++ d1/d2/",
        "cL+++++++++ link1",
        "cS+++++++++ afifo",
    ] {
        assert!(
            stdout.contains(row),
            "upstream itemize must include `{row}`; got:\n{stdout}"
        );
    }
    for frag in ["dir: 2", "link: 1", "special: 1"] {
        assert!(
            stdout.contains(frag),
            "upstream --stats created breakdown must include `{frag}`; got:\n{stdout}"
        );
    }
}
