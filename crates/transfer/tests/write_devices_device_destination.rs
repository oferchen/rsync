//! `--write-devices` must stream a regular source into an existing DEVICE
//! destination instead of truncating it.
//!
//! The receiver's device predicate is keyed on the DESTINATION, never on the
//! sender's file-list entry: `--write-devices` writes a REGULAR file's contents
//! into an EXISTING device node, so the flist entry describes a regular file by
//! construction. Keying the predicate on the entry left it permanently false,
//! which let the in-place commit run `ftruncate()` against the device and fail
//! `EINVAL`, reporting exit 12 where upstream reports 0.
//!
//! upstream: `receiver.c:1170` - `write_to_device = write_devices && IS_DEVICE(st.st_mode)`,
//! with `st` the `do_fstat()` of the opened destination (`receiver.c:1143-1145`).
//! upstream: `receiver.c:652` - the in-place `do_ftruncate()` is gated on
//! `!IS_DEVICE(file->mode)`, which is the truncate this predicate must suppress.
//!
//! Why this rides a REMOTE-SHELL push and not a local copy: the predicate lives
//! on the receiver pipeline (`crates/transfer/src/receiver/transfer/pipeline.rs`),
//! and a plain local `oc-rsync src /dev/null` never reaches it - that is the
//! engine crate's local-copy executor, which has no `--write-devices` predicate
//! at all. A local fixture passes on the unfixed tree and proves nothing.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// A character device every Unix has, which `--write-devices` may be pointed at
/// without destroying anything: the device branch opens it `O_WRONLY` and never
/// unlinks or renames over it, at any privilege level.
const DEVICE_DEST: &str = "/dev/null";

/// Reports whether this platform rejects `ftruncate()` against a device node.
///
/// The defect is only OBSERVABLE where truncating a device fails. Linux returns
/// `EINVAL`; macOS accepts the call silently, so the same transfer succeeds
/// there whether or not the predicate is correct, and asserting on it would be
/// a vacuous pass. Probing the behaviour beats hardcoding a target list: the
/// test then runs exactly where it can discriminate.
fn platform_rejects_truncating_a_device() -> bool {
    let Ok(file) = fs::OpenOptions::new().write(true).open(DEVICE_DEST) else {
        return false;
    };
    file.set_len(1).is_err()
}

fn device_dest_is_usable() -> Result<(), String> {
    let meta = fs::metadata(DEVICE_DEST).map_err(|e| format!("cannot stat {DEVICE_DEST}: {e}"))?;
    if !std::os::unix::fs::FileTypeExt::is_char_device(&meta.file_type()) {
        return Err(format!("{DEVICE_DEST} is not a character device"));
    }
    if !platform_rejects_truncating_a_device() {
        return Err(format!(
            "this platform accepts ftruncate() on {DEVICE_DEST}, so the \
             truncate-against-a-device defect is not observable here"
        ));
    }
    Ok(())
}

fn write_source(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("payload");
    fs::write(&src, b"write-devices payload 0123456789\n").expect("write source");
    src
}

/// Regression: a `--write-devices` push to a device destination must exit 0.
///
/// On the unfixed tree the receiver's predicate read the sender's file-list
/// entry (a regular file), so `is_device_target` stayed false, the in-place
/// commit truncated the device and the server failed `EINVAL` - surfacing to the
/// client as exit 12. Upstream 3.5.0 exits 0 on the same invocation.
#[test]
fn write_devices_push_to_a_device_destination_succeeds() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    if let Err(reason) = device_dest_is_usable() {
        eprintln!("SKIP write_devices_push_to_a_device_destination_succeeds: {reason}");
        return;
    }
    let stub = LshRunnerStub::locate().expect("lsh-stub located");

    let tmp = create_tempdir();
    let src = write_source(tmp.path());

    OcRsyncCliRunner::new()
        .arg("--write-devices")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(src.display().to_string())
        .arg(format!("localhost:{DEVICE_DEST}"))
        .run()
        .expect("push run")
        .assert_success();

    // The device branch writes THROUGH the node; it must never have been
    // replaced by a regular file: under `inplace` upstream names the output
    // `fname` itself (receiver.c:1195-1196), so there is no temp file to rename
    // over the device and no unlink of it.
    let meta = fs::metadata(DEVICE_DEST).expect("stat device after transfer");
    assert!(
        std::os::unix::fs::FileTypeExt::is_char_device(&meta.file_type()),
        "{DEVICE_DEST} must still be a character device after --write-devices"
    );
}

/// Companion: the same push WITHOUT a device destination must stay correct.
///
/// Without this, the regression test above would still pass if `--write-devices`
/// had been made to short-circuit every transfer. It pins that a regular-file
/// destination is unaffected by the predicate change - the arm that must keep
/// using temp-file staging and the closing `set_len`.
#[test]
fn write_devices_push_to_a_regular_file_still_transfers_content() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let stub = LshRunnerStub::locate().expect("lsh-stub located");

    let tmp = create_tempdir();
    let src = write_source(tmp.path());
    let dst = tmp.path().join("regular-dest");

    OcRsyncCliRunner::new()
        .arg("--write-devices")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(src.display().to_string())
        .arg(format!("localhost:{}", dst.display()))
        .run()
        .expect("push run")
        .assert_success();

    assert_eq!(
        fs::read(&src).expect("read source"),
        fs::read(&dst).expect("read destination"),
        "a regular-file destination must receive the source bytes verbatim"
    );
}
