//! `--write-devices` is a DECISION on the LOCAL-COPY path, not a coincidence.
//!
//! Upstream keys the flag on the DESTINATION in exactly two places:
//!
//! ```text
//! generator.c:2148-2153                      (a regular file is arriving)
//! if (statret == 0 && !(stype == FT_REG || (write_devices && stype == FT_DEVICE))) {
//!         if (delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_FILE) != 0)
//!                 goto cleanup;
//!         statret = -1;
//!         stat_errno = ENOENT;
//! }
//!
//! receiver.c:1170                            (the write itself)
//! write_to_device = write_devices && IS_DEVICE(st.st_mode);
//! ```
//!
//! The source is a REGULAR file in both arms - `--write-devices` streams a
//! regular file's contents into an existing device node - so nothing in the
//! entry being sent identifies the case. `tests/../crates/transfer` pins the
//! receiver twin (PR #7583); the local-copy executor had no `--write-devices`
//! predicate at all and reached the right answer only because its DEFAULT write
//! strategy stages into a temp file and renames over whatever is there. Every
//! strategy that opens the final name instead - `--inplace`, `--append`, and
//! the `--inplace` that `--write-devices` itself implies (`options.c:2555`) -
//! wrote THROUGH a device node the operator had not authorised writing through.
//!
//! # Measured against rsync 3.5.0
//!
//! Local `rsync SRC DST` as root on Linux (aarch64, tmpfs mounted `dev`, the
//! destination a loop device over a 64 KiB backing file so the bytes that reach
//! the device are readable afterwards):
//!
//! | cell | rsync 3.5.0 | oc before |
//! |---|---|---|
//! | `--inplace`, device dest | 0, dest is a REGULAR file | 0, dest still a device, written through |
//! | `--append`, device dest | 0, dest is a REGULAR file | 0, dest still a device, written through |
//! | `--inplace -r`, device inside the tree | 0, dest is a REGULAR file | 0, dest still a device |
//! | `-i`, device dest | `>f+++++++++` | `>f.sT......` |
//! | `--stats`, device dest | `created files: 1` | `created files: 0` |
//! | `--backup --inplace`, device dest | 0, dest REGULAR, `dst~` the device | 0, dest still the device |
//! | `--write-devices`, device dest | 0, device kept and written | 0, device kept and written |
//! | `--ignore-existing`, device dest | 0, device untouched | 0, device untouched |
//!
//! # Why most cells need root
//!
//! `mknod(2)` is privileged on Linux and macOS alike, and the arm that must be
//! proved is the one where the node is DESTROYED - which rules out pointing it
//! at `/dev/null`. Those cells therefore self-gate on being able to create a
//! device node and say so out loud when they cannot; the `/dev/null` cell below
//! runs everywhere and pins the opposite arm, where nothing is removed.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const PAYLOAD: &str = "write-devices payload 0123456789\n";
const SIBLING: &str = "sibling payload\n";

/// A character device every Unix has, which `--write-devices` may be pointed at
/// without destroying anything: the keep arm opens it and never unlinks or
/// renames over it, at any privilege level.
const SHARED_DEVICE: &str = "/dev/null";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

/// Creates a character device node with `/dev/null`'s numbers.
///
/// Shells out rather than adding an FFI call to the test tree, matching the
/// `mkfifo(1)` shell-out `tests/local_directory_obstacle_removal.rs` uses.
fn make_char_device(path: &Path) -> Result<(), String> {
    for program in ["mknod", "/sbin/mknod", "/usr/bin/mknod"] {
        match Command::new(program)
            .arg(path)
            .args(["c", "1", "3"])
            .output()
        {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => {
                return Err(format!(
                    "{program} refused to create a device node: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Err(_) => continue,
        }
    }
    Err("no mknod(1) on this host".to_owned())
}

/// Builds `src/{entry,sibling}` and `dst/entry` as a character device.
///
/// `sibling` separates "the device entry was skipped" from "the whole transfer
/// stopped", the same role it plays in the directory-obstacle fixture.
///
/// Returns `None` when this host cannot create a device node; the caller then
/// reports the skip rather than passing vacuously.
fn device_fixture() -> Option<Fixture> {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    fs::write(root.join("src/entry"), PAYLOAD).expect("write source payload");
    fs::write(root.join("src/sibling"), SIBLING).expect("write source sibling");

    match make_char_device(&root.join("dst/entry")) {
        Ok(()) => Some(Fixture { _temp: temp, root }),
        Err(reason) => {
            eprintln!("SKIP: cannot plant a device destination here: {reason}");
            None
        }
    }
}

/// Copies `src/` onto `dst/` with a PLAIN LOCAL invocation - no `--rsh`, no
/// `--server` child - so the engine's local-copy executor is under test.
fn copy(fx: &Fixture, extra: &[&str]) -> (Option<i32>, String, String) {
    let out = test_support::OcRsyncCliRunner::new()
        // -I defeats the quick check so the entry is always reconsidered.
        .args(["-rI"])
        .args(extra)
        .arg(format!("{}/src/", fx.root.display()))
        .arg(format!("{}/dst/", fx.root.display()))
        .run()
        .expect("copy did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn entry(fx: &Fixture) -> PathBuf {
    fx.root.join("dst/entry")
}

fn is_char_device(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_char_device())
}

fn sibling_landed(fx: &Fixture) -> bool {
    fs::read_to_string(fx.root.join("dst/sibling")).is_ok_and(|body| body == SIBLING)
}

/// The headline. `--inplace` WITHOUT `--write-devices` must not write through a
/// device destination: upstream clears the node and creates an ordinary file.
///
/// This is the cell the default write strategy hides. Without `--inplace` the
/// executor stages into a temp file and renames over the node, which lands on
/// upstream's answer by accident; with it the executor opened the final name
/// and streamed the payload straight into the device, leaving the node in place
/// and the operator's file unwritten at the destination.
///
/// upstream: `generator.c:2148` - `write_devices` is false here, so a
/// `stype == FT_DEVICE` destination fails the keep condition and
/// `delete_item(..., DEL_FOR_FILE)` clears it.
#[test]
fn inplace_without_write_devices_replaces_a_device_destination() {
    let Some(fx) = device_fixture() else { return };

    let (status, stdout, stderr) = copy(&fx, &["--inplace"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(
        fs::read_to_string(entry(&fx)).unwrap_or_default(),
        PAYLOAD,
        "upstream removes the device node and writes an ordinary file at that \
         name; oc wrote the payload INTO the device and left the node standing"
    );
    assert!(
        !is_char_device(&entry(&fx)),
        "the destination must no longer be a device node"
    );
    assert!(sibling_landed(&fx), "the rest of the batch must still land");
}

/// `--append` reaches the same open-the-final-name strategy and so needs the
/// same decision. Keeping it separate from the `--inplace` cell above pins that
/// the predicate lives in the make-way decision rather than in one strategy.
///
/// upstream: `generator.c:2148`; `options.c:2555` is what makes `--append` and
/// `--write-devices` share the in-place write, which is exactly why the device
/// question cannot be answered from `inplace` alone.
#[test]
fn append_without_write_devices_replaces_a_device_destination() {
    let Some(fx) = device_fixture() else { return };

    let (status, stdout, stderr) = copy(&fx, &["--append"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert!(
        !is_char_device(&entry(&fx)),
        "upstream replaces the node; oc appended into the device instead. \
         stdout: {stdout} stderr: {stderr}"
    );
    assert_eq!(fs::read_to_string(entry(&fx)).unwrap_or_default(), PAYLOAD);
}

/// The removal is also visible without any write-strategy flag at all, because
/// upstream resets `statret` to `-1` afterwards: the entry itemizes as a
/// CREATION, not as an update against a destination that is no longer there.
///
/// This cell fails on a tree that removes the node but forgets the reset, which
/// no filesystem assertion above can catch.
///
/// upstream: `generator.c:2151-2152` - `statret = -1; stat_errno = ENOENT;`
#[test]
fn a_replaced_device_destination_itemizes_as_a_creation() {
    let Some(fx) = device_fixture() else { return };

    let (status, stdout, stderr) = copy(&fx, &["-i"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert!(
        stdout.contains(">f+++++++++ entry"),
        "upstream itemizes the entry as a creation once the device is cleared; \
         a surviving destination reports an update instead. stdout: {stdout}"
    );
}

/// `--backup` disposes of the node instead of unlinking it: `delete_item()`
/// takes the `make_backup(fbuf, True)` arm for a NON-directory, and the `True`
/// is load-bearing - the generator's own `make_backup(fname, False)` tries a
/// hard link first, which would leave the device standing at the destination
/// and let the transfer write through it after all.
///
/// upstream: `delete.c:227-238`.
#[test]
fn a_replaced_device_destination_is_backed_up_not_unlinked() {
    let Some(fx) = device_fixture() else { return };

    let (status, stdout, stderr) = copy(&fx, &["--inplace", "--backup"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert!(
        !is_char_device(&entry(&fx)),
        "the destination must be an ordinary file after the backup moved the \
         node aside. stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        is_char_device(&fx.root.join("dst/entry~")),
        "the device NODE itself must be what landed in the backup"
    );
}

/// The negative control that stops the predicate degenerating into "always
/// clear": with `--write-devices` the node survives and is written through.
///
/// upstream: `generator.c:2148` - the `write_devices && stype == FT_DEVICE`
/// half keeps the node; `receiver.c:1170` then writes into it.
#[test]
fn write_devices_keeps_a_device_destination() {
    let Some(fx) = device_fixture() else { return };

    let (status, stdout, stderr) = copy(&fx, &["--write-devices"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert!(
        is_char_device(&entry(&fx)),
        "--write-devices exists to keep the node; clearing it here would make \
         the option a no-op. stdout: {stdout} stderr: {stderr}"
    );
}

/// The ordering control. `--ignore-existing` is tested at `generator.c:1780`,
/// BEFORE either make-way removal, so it leaves the device standing even though
/// `--write-devices` was not given.
#[test]
fn ignore_existing_leaves_a_device_destination_alone() {
    let Some(fx) = device_fixture() else { return };

    let (status, stdout, stderr) = copy(&fx, &["--inplace", "--ignore-existing"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert!(
        is_char_device(&entry(&fx)),
        "--ignore-existing is evaluated against the PRE-removal destination, so \
         the node must survive untouched. stdout: {stdout} stderr: {stderr}"
    );
}

/// The UNPRIVILEGED cell, and the only one of these that runs in an ordinary
/// CI job: no device node is created, so it needs nothing but `/dev/null`.
///
/// It pins the keep arm by its diagnostics rather than by its exit code. A tree
/// whose predicate ignores `--write-devices` reaches `delete_item()` here, and
/// the kernel refuses the `unlink` of a node in `/dev` for a non-root caller -
/// so nothing is destroyed, the run says `could not make way for new regular
/// file` out loud, and that line is the observable. Asserting on the node's
/// type alone would not separate the two trees at all.
///
/// **That refusal is the whole safety argument, so the cell runs only where it
/// holds.** The gate is the complement of the privileged cells above: a host
/// that CAN create a device node can also unlink one out of `/dev`, and a
/// mutated tree run there would destroy the host's `/dev/null` for real - which
/// is what happened once while this file was being written. Those hosts get
/// their coverage from `write_devices_keeps_a_device_destination`, which asks
/// the same question of a node inside the fixture.
///
/// The exit code is deliberately NOT asserted. `--write-devices` into a device
/// this user does not own exits 23 on oc with `failed to apply dest_mode`,
/// where rsync 3.5.0 exits 0 (measured on macOS as an unprivileged user), and
/// that is a SEPARATE defect: upstream's `set_file_attrs()` skips the `chmod`
/// when the permission bits already match (`rsync.c` - `if
/// (!BITS_EQUAL(st->st_mode, new_mode, CHMOD_BITS))`), and `dest_mode()`'s
/// `exists` arm has already made them match. Its cause is the unconditional
/// `chmod`, not this predicate, and it is not fixed here; folding it into the
/// assertion would tie this cell to that defect's lifetime.
#[test]
fn write_devices_never_makes_way_at_a_shared_device() {
    if !is_char_device(Path::new(SHARED_DEVICE)) {
        eprintln!("SKIP: {SHARED_DEVICE} is not a character device on this host");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    if make_char_device(&temp.path().join("privilege-probe")).is_ok() {
        eprintln!(
            "SKIP: this host can create device nodes, so it can also unlink \
             {SHARED_DEVICE} - the privileged cells cover the keep arm here"
        );
        return;
    }

    let src = temp.path().join("payload");
    fs::write(&src, PAYLOAD).expect("write source");

    let out = test_support::OcRsyncCliRunner::new()
        .arg("--write-devices")
        .arg(src.display().to_string())
        .arg(SHARED_DEVICE)
        .run()
        .expect("copy did not finish");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        !stderr.contains("could not make way"),
        "--write-devices must keep the node, so no make-way removal may be \
         attempted at all. stderr: {stderr}"
    );
    assert!(
        !stderr.contains("delete_file: unlink"),
        "the keep arm must not reach delete_item()'s unlink. stderr: {stderr}"
    );
    assert!(
        is_char_device(Path::new(SHARED_DEVICE)),
        "the keep arm must never unlink or rename over the node"
    );
}
