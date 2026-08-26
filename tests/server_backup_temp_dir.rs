//! `--backup-dir` and `--temp-dir` must survive the `--server` argument decoder.
//!
//! Both were recognised only well enough to keep their value slot out of the
//! positional operand list, and then discarded - the decoder had an explicit
//! no-op arm whose own doc comment said the fields were not consumed. The
//! consequences are not cosmetic:
//!
//! - `-b --backup-dir=DIR` silently degraded to in-place SUFFIX backups, a
//!   different on-disk layout from the one the operator asked for.
//! - `--temp-dir=DIR` was ignored entirely, so a staging directory chosen to
//!   keep large temporaries off the destination filesystem simply did not take
//!   effect.
//!
//! upstream emits both through `safe_arg()` in `server_options()`
//! (`options.c:2807-2808` and `:2926-2927`), and its receiver honours them.
//!
//! ⚠ These cells force a REAL external `--server` process via `--rsync-path`.
//! The in-process embedded-ssh path builds its `ServerConfig` straight from the
//! client's config (`embedded_ssh_transfer.rs`), so it carries both values
//! across without ever consulting the decoder and MASKS the defect completely.
//! A test written the obvious way passes while exercising nothing.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Running as root defeats a permission-based fixture: root ignores the mode
/// bits, so the unwritable-temp-dir cells would report the pre-fix behaviour on
/// a fixed binary. Skip rather than emit a false signal on the root CI leg.
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

/// `src/f` holds new content; `dst/f` holds older, backdated content, so the
/// quick check sees a genuine update and the receiver has something to back up.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    fs::write(root.join("src/f"), b"new content here").expect("write src");
    fs::write(root.join("dst/f"), b"old").expect("write dst");
    filetime::set_file_mtime(
        root.join("dst/f"),
        filetime::FileTime::from_unix_time(1_000_000, 0),
    )
    .expect("backdate dst");
    let shim = write_rsh_shim(&root);
    Fixture {
        _temp: temp,
        root,
        shim,
    }
}

/// Runs a push through the shim against an external `--server` and reports
/// (exit code, destination entries sorted).
fn push(fx: &Fixture, extra: &[String]) -> (Option<i32>, Vec<String>) {
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .args(["-r", "-b", "-I"])
        .args(extra)
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/src/", fx.root.display()))
        .arg(format!("bkhost:{}/dst/", fx.root.display()))
        .run()
        .expect("push did not finish");

    let dst = fx.root.join("dst");
    let mut found = Vec::new();
    let mut stack = vec![dst.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read dst") {
            let path = entry.expect("dir entry").path();
            found.push(
                path.strip_prefix(&dst)
                    .expect("under dst")
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    found.sort();
    (out.status, found)
}

/// The headline: the backup must land UNDER the named directory.
#[test]
fn backup_dir_places_the_backup_under_the_named_directory() {
    let fx = fixture();
    let (status, entries) = push(&fx, &["--backup-dir=bak".to_string()]);
    assert_eq!(status, Some(0), "the transfer itself must still succeed");
    assert_eq!(
        entries,
        vec!["bak".to_string(), "bak/f".to_string(), "f".to_string()],
        "with --backup-dir the previous version belongs at bak/f; an `f~` here \
         means the decoder discarded the directory and silently fell back to \
         suffix backups"
    );
}

/// Non-vacuity for the cell above: the same fixture DOES produce a backup when
/// no directory is named, so "bak/f exists" cannot be passing merely because
/// the fixture always backs up somewhere.
#[test]
fn without_backup_dir_the_backup_keeps_the_suffix_form() {
    let fx = fixture();
    let (status, entries) = push(&fx, &[]);
    assert_eq!(status, Some(0));
    assert_eq!(
        entries,
        vec!["f".to_string(), "f~".to_string()],
        "bare -b is the suffix form; if this stopped producing a backup the \
         --backup-dir cell would prove nothing"
    );
}

/// `--temp-dir` shares the decoder's no-op arm, so it shared the defect. An
/// unwritable staging directory is the discriminator: honoured means the file
/// cannot be staged and fails, ignored means it lands anyway.
///
/// Measured against real rsync 3.5.0: `rc=23`, destination file absent.
#[test]
fn temp_dir_is_honoured_so_an_unwritable_one_fails_the_file() {
    if running_as_root() {
        return;
    }
    let fx = fixture();
    // Staging lives INSIDE the destination tree. oc confines the staging
    // family (--temp-dir/--partial-dir/--backup-dir) against the destination
    // root, so an absolute --temp-dir outside it is refused before the mode
    // bits are ever consulted - which would make the permission bit a
    // non-discriminator and this pair vacuous. Measured on Linux: outside the
    // tree BOTH the writable and unwritable cases return 23 (mkstemp EACCES);
    // inside it, writable returns 0 and unwritable returns 23, which is the
    // contrast these two cells exist to draw.
    let tmp = fx.root.join("dst").join("staging");
    fs::create_dir_all(&tmp).expect("create staging");
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500)).expect("chmod staging");

    let (status, _) = push(&fx, &[format!("--temp-dir={}", tmp.display())]);

    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).expect("restore staging");
    assert_eq!(
        status,
        Some(23),
        "upstream 3.5.0 exits 23 here; exit 0 means --temp-dir was ignored and \
         the file was staged next to the destination instead"
    );
}

/// Non-vacuity for the cell above: the same invocation with a WRITABLE staging
/// directory succeeds, so the failure is attributable to the permission bit and
/// not to `--temp-dir` being rejected outright.
#[test]
fn a_writable_temp_dir_still_completes_the_transfer() {
    if running_as_root() {
        return;
    }
    let fx = fixture();
    // Staging lives INSIDE the destination tree. oc confines the staging
    // family (--temp-dir/--partial-dir/--backup-dir) against the destination
    // root, so an absolute --temp-dir outside it is refused before the mode
    // bits are ever consulted - which would make the permission bit a
    // non-discriminator and this pair vacuous. Measured on Linux: outside the
    // tree BOTH the writable and unwritable cases return 23 (mkstemp EACCES);
    // inside it, writable returns 0 and unwritable returns 23, which is the
    // contrast these two cells exist to draw.
    let tmp = fx.root.join("dst").join("staging");
    fs::create_dir_all(&tmp).expect("create staging");

    let (status, entries) = push(&fx, &[format!("--temp-dir={}", tmp.display())]);
    assert_eq!(status, Some(0));
    assert!(
        entries.contains(&"f".to_string()),
        "a writable --temp-dir must not change the outcome"
    );
}
