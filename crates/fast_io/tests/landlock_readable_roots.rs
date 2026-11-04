//! Basis roots get the least each alt-dest kind needs, and never the right
//! to create or remove anything.
//!
//! The `--server` receiver installs its `--compare-dest` / `--copy-dest`
//! directories as readable roots and its `--link-dest` directories as
//! link-source roots (upstream: generator.c:1073 `try_dests_reg()`). It opens
//! a basis there, copies from there, or hard-links out of there into the
//! destination. A rule granting the full rights the destination gets would
//! let a raced-in symlink turn a basis read into a write outside the
//! transfer.

#![cfg(all(target_os = "linux", feature = "landlock"))]

use fast_io::landlock::{LandlockOutcome, is_supported, restrict_to_paths};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::thread;

/// Landlock restricts the calling thread irreversibly, so the scenario runs
/// on a worker that exits afterwards; the caller owns the tempdir and drops
/// it outside the sandbox.
fn in_sandbox<F>(scenario: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    thread::Builder::new()
        .name("landlock-basis-roots".into())
        .spawn(scenario)
        .map_err(|e| format!("spawn worker: {e}"))?
        .join()
        .map_err(|_| "worker thread panicked".to_owned())?
}

fn expect_denied<T>(result: std::io::Result<T>, what: &str) -> Result<(), String> {
    match result {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => Ok(()),
        Err(e) => Err(format!("{what}: expected EACCES, got {e}")),
        Ok(_) => Err(format!("{what}: succeeded under a basis rule")),
    }
}

/// Neither kind of basis root may gain or lose an entry.
fn expect_no_namespace_change(root: &Path, kind: &str) -> Result<(), String> {
    expect_denied(
        fs::write(root.join("planted"), b"X"),
        &format!("create in {kind}"),
    )?;
    expect_denied(
        fs::remove_file(root.join("basis")),
        &format!("unlink in {kind}"),
    )?;
    expect_denied(
        fs::create_dir(root.join("sub")),
        &format!("mkdir in {kind}"),
    )?;
    expect_denied(
        fs::rename(root.join("basis"), root.join("moved")),
        &format!("rename in {kind}"),
    )
}

#[test]
fn basis_roots_are_read_or_linked_from_but_never_reshaped() {
    if !is_supported() {
        return;
    }
    let temp = test_support::create_tempdir();
    let dest = temp.path().join("dest");
    let compare = temp.path().join("compare");
    let link = temp.path().join("link");
    for dir in [&dest, &compare, &link] {
        fs::create_dir(dir).expect("mkdir");
    }
    fs::write(compare.join("basis"), b"BASIS").expect("write compare basis");
    fs::write(link.join("basis"), b"BASIS").expect("write link basis");

    let (dest, compare, link): (PathBuf, PathBuf, PathBuf) = (dest, compare, link);
    in_sandbox(move || {
        match restrict_to_paths(&[dest.as_path()], &[compare.as_path()], &[link.as_path()]) {
            LandlockOutcome::Enforced(_) => {}
            other => return Err(format!("sandbox did not engage: {other:?}")),
        }

        // Non-vacuity: the destination root is writable, so each denial
        // below is a basis rule at work, not a sandbox denying everything.
        fs::write(dest.join("written"), b"OK").map_err(|e| format!("write in dest: {e}"))?;

        let read = fs::read(compare.join("basis")).map_err(|e| format!("read basis: {e}"))?;
        if read != b"BASIS" {
            return Err(format!("wrong basis content: {read:?}"));
        }
        expect_denied(
            fs::OpenOptions::new()
                .write(true)
                .open(compare.join("basis")),
            "open a compare/copy basis for writing",
        )?;
        // A read-only root is no link source: Landlock refuses the reparent
        // with EXDEV because the file would gain write rights in `dest`.
        match fs::hard_link(compare.join("basis"), dest.join("from-compare")) {
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {}
            other => return Err(format!("hard link out of a read-only root: {other:?}")),
        }
        expect_no_namespace_change(&compare, "a compare/copy root")?;

        fs::hard_link(link.join("basis"), dest.join("linked"))
            .map_err(|e| format!("link-dest hard link into the destination: {e}"))?;
        expect_no_namespace_change(&link, "a link-dest root")
    })
    .expect("sandboxed scenario");

    drop(temp);
}
