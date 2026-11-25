//! Cross-implementation oracle for `--write-batch` iflags fidelity of
//! created directories, symlinks, and special files: an upstream rsync
//! `--read-batch` replay of an oc-produced batch must itemize each created
//! non-regular entry (`cd`/`cL`/`cS`) and count it in `--stats`.
//!
//! Needs the rsync 3.5.2 oracle, resolved by
//! `test_support::require_upstream_rsync` (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, or
//! the install `tools/ci/run_interop.sh build-only` makes). Without it the
//! test prints a skip line and returns; under `OC_RSYNC_UPSTREAM_COMPAT=1`,
//! as the interop job's oracle step runs it, a missing oracle fails.
//!
//! The always-run companion `write_batch_nonfile_iflags.rs` pins the same
//! defect at the byte level without upstream; this test additionally confirms
//! the itemize rows and `--stats` created breakdown upstream actually prints.
//!
//! # Upstream Reference
//!
//! - `generator.c:1869-1872`/`:2003-2009`/`:2093-2096 itemize()`,
//!   `receiver.c:918-971` (`stats.created_{dirs,symlinks,specials}`),
//!   `receiver.c:738 no_batched_update()` (a missing special entry aborts
//!   the replay with exit 23).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;
use test_support::{Deadlined, UpstreamVersion, run_deadlined};

/// Wall-clock budget for the upstream `--read-batch` subprocess.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn upstream_read_batch_counts_created_dirs_symlinks_and_specials() {
    let Some(oracle) = test_support::require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let upstream = oracle.binary();
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

    let mut read_cmd = Command::new(upstream);
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
