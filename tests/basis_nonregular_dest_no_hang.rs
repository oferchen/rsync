//! A non-regular delta basis must never park the receiver in `open(2)`.
//!
//! A FIFO with no writer blocks `open(O_RDONLY)` in `fifo_open()` until a
//! writer arrives. The receiver's basis lookup opens whatever it finds at the
//! destination, so a FIFO planted there wedges the transfer with no timeout and
//! no flag required - the same `oc-rsync --server` receiver a writable daemon
//! module runs.
//!
//! Upstream 3.5.0 never reaches its own basis open with a non-regular
//! `fnamecmp`. `generator.c:2148` removes the obstacle first
//! (`if (statret == 0 && !(stype == FT_REG || (write_devices && stype ==
//! FT_DEVICE))) { delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_FILE);
//! statret = -1; }`), and the alt-dest, fuzzy and `--partial-dir` candidates
//! are each gated on `S_ISREG` (`generator.c:1084`, `generator.c:860,888`,
//! `generator.c:2175`). By the time `do_open_checklinks(fnamecmp)` runs at
//! `generator.c:2313` the basis is always a regular file.
//!
//! oc does not have that removal, so it states the invariant at the open
//! instead: `fast_io::open_basis_nofollow` refuses a non-regular node, and the
//! receiver falls back to a full send. The observable result matches
//! upstream's: exit 0, and the destination replaced by a regular file holding
//! the source bytes.
//!
//! # Two call sites, not one
//!
//! `fast_io::open_basis_nofollow` has two production callers in
//! `crates/transfer/src/receiver/basis.rs`: `try_open_file` (the exact
//! destination, the `--partial-dir` fallback and the `--fuzzy` candidate) and
//! `try_reference_directories` (the `--compare-dest` / `--copy-dest` /
//! `--link-dest` lookup, which checked `is_file()` only *after* the open, so it
//! parked too). Both are exercised here.
//!
//! # Non-vacuity
//!
//! Each case bounds the client with a deadline and asserts the *positive*
//! outcome: the destination ends up a regular file holding the source bytes. A
//! test that only checked "did not hang" would pass on an outright failure. The
//! FIFO-ness of the fixture is asserted before the run, so a `mkfifo` that
//! silently produced a regular file cannot make the case inert. Before the fix
//! both cases time out.
//!
//! # Known remaining
//!
//! `--inplace` and `--write-devices` still park on a FIFO destination, at a
//! different site: `fast_io::inplace_open::InplaceResolution::open_write`
//! (`crates/fast_io/src/inplace_open.rs`) issues `O_WRONLY|O_CREAT`, which
//! blocks on a FIFO waiting for a *reader*. That is not a basis open, and
//! refusing it would not reach upstream's answer either (upstream succeeds with
//! exit 0 by having deleted the FIFO at `generator.c:2148`). It needs that
//! removal, which is a separate change.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Generous enough for a debug-build local wire transfer of nineteen bytes,
/// short enough that a parked `open(2)` is reported as a failure rather than
/// wedging the run. Before the fix the receiver never returns at all, so any
/// finite bound distinguishes the two states.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

const PAYLOAD: &[u8] = b"non-regular-basis-payload\n";

/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read with
/// `env!`: at run time it is unset and a lookup would fall through to whatever
/// stale `target/debug/oc-rsync` happens to be on disk.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// A `--rsh` shim that drops the host argument and execs the rest locally, so
/// the transfer takes the real `--server` receiver path (a plain local copy
/// goes through the local-copy executor and never reaches the basis lookup).
fn write_rsh_shim(root: &Path) -> PathBuf {
    let shim = root.join("rsh.sh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    let mut perms = fs::metadata(&shim).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&shim, perms).expect("chmod shim");
    shim
}

fn make_fifo(path: &Path) {
    // `mkfifo(1)` rather than a new libc dependency, matching the convention in
    // tests/drop_devices.rs.
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("spawn mkfifo");
    assert!(
        status.success(),
        "mkfifo {} failed: {status}",
        path.display()
    );
    let file_type = fs::symlink_metadata(path)
        .expect("stat planted node")
        .file_type();
    assert!(
        file_type.is_fifo(),
        "fixture is inert: {} is not a FIFO",
        path.display()
    );
}

/// Runs `cmd` under a deadline. Returns `None` when the deadline expires, which
/// is how the pre-fix park is reported instead of hanging the suite.
fn run_with_deadline(mut cmd: Command) -> Option<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync");
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait().expect("poll child") {
            Some(_) => return Some(child.wait_with_output().expect("collect output")),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// Asserts the transfer completed inside the deadline, reported success, and
/// left the source bytes at `dest` as a regular file.
fn assert_transfer_landed(output: Option<Output>, dest: &Path, case: &str) {
    let Some(output) = output else {
        panic!(
            "{case}: oc-rsync did not exit within {RUN_TIMEOUT:?} - the receiver \
             parked opening a non-regular destination as the delta basis"
        );
    };
    assert!(
        output.status.success(),
        "{case}: expected exit 0 (upstream 3.5.0 exits 0 here), got {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let meta = fs::symlink_metadata(dest)
        .unwrap_or_else(|error| panic!("{case}: destination {} missing: {error}", dest.display()));
    assert!(
        meta.file_type().is_file(),
        "{case}: destination {} is {:?}, not a regular file",
        dest.display(),
        meta.file_type()
    );
    let landed = fs::read(dest).expect("read destination");
    assert_eq!(
        landed, PAYLOAD,
        "{case}: destination content does not match the source"
    );
}

/// `try_open_file(config.file_path)` - the exact destination, the default path,
/// no flag required. Upstream deletes the FIFO at `generator.c:2148` and writes
/// a fresh regular file; oc declines it as a basis and does the same.
#[test]
fn fifo_destination_does_not_park_the_receiver() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::write(src.join("obj"), PAYLOAD).expect("write source");
    make_fifo(&dst.join("obj"));

    let shim = write_rsh_shim(root);
    let binary = oc_rsync_binary();
    let mut cmd = Command::new(&binary);
    cmd.arg("-e")
        .arg(&shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(src.join("obj"))
        .arg(format!("localhost:{}/", dst.display()));

    assert_transfer_landed(run_with_deadline(cmd), &dst.join("obj"), "fifo destination");
}

/// `try_reference_directories` - the second caller. Its `is_file()` check ran
/// *after* the open, so a FIFO in a `--compare-dest` directory parked the
/// lookup just as the destination one did. Upstream's `try_dests_reg()` never
/// opens it: `generator.c:1084` rejects the candidate on
/// `!S_ISREG(sxp->st.st_mode)`.
#[test]
fn fifo_in_compare_dest_does_not_park_the_basis_lookup() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let src = root.join("src");
    let dst = root.join("dst");
    let reference = root.join("ref");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::create_dir_all(&reference).expect("mkdir ref");
    fs::write(src.join("obj"), PAYLOAD).expect("write source");
    make_fifo(&reference.join("obj"));

    let shim = write_rsh_shim(root);
    let binary = oc_rsync_binary();
    let mut cmd = Command::new(&binary);
    cmd.arg(format!("--compare-dest={}", reference.display()))
        .arg("-e")
        .arg(&shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(src.join("obj"))
        .arg(format!("localhost:{}/", dst.display()));

    assert_transfer_landed(
        run_with_deadline(cmd),
        &dst.join("obj"),
        "fifo in --compare-dest",
    );

    // The reference directory is read-only input: the run must not have
    // replaced the node it declined.
    assert!(
        fs::symlink_metadata(reference.join("obj"))
            .expect("stat reference node")
            .file_type()
            .is_fifo(),
        "the --compare-dest node must be left untouched"
    );
}
