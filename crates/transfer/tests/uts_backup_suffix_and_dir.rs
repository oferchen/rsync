//! Nextest port of the upstream `testsuite/backup.test` scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/backup.test`.
//!
//! # Background
//!
//! Upstream's `backup.test` verifies `--backup`: before overwriting a changed
//! destination file, rsync preserves the old version. Two placement modes
//! exist and this test pins both:
//!
//! - In-place suffix: the old file is renamed beside the new one with the
//!   backup suffix (default `~`, overridable with `--suffix`). Upstream drives
//!   this leg with `--no-whole-file --backup` and greps for
//!   `backed up name to name~`.
//! - Backup directory: with `--backup-dir=DIR` the old version is relocated
//!   under `DIR` at the same relative path, and the `~` suffix is dropped.
//!   Upstream greps for `backed up name to .../name`.
//!
//! # Why this matters
//!
//! Backup is a destructive-avoidance feature: a regression that skips the
//! rename, writes to the wrong path, applies the suffix in backup-dir mode, or
//! omits the `--info=backup` message silently loses the user's prior data or
//! breaks scripts that parse the backup log. The suffix-vs-directory split is
//! two genuinely different placement paths in the generator, so both are
//! exercised. The `backed up X to Y` wording must match upstream byte-for-byte
//! (`backup.c:353`) because tooling greps for it.
//!
//! # What this test pins
//!
//! 1. Default `~` suffix: after an update the new content lands, `name~` holds
//!    the prior content, and `backed up name to name~` is emitted.
//! 2. Custom `--suffix=.bak`: same, with `name.bak` as the backup.
//! 3. `--backup-dir`: the prior content is relocated under the backup dir at
//!    the same relative name (no `~`), and the message names that path.
//!
//! # Upstream References
//!
//! - `testsuite/backup.test` - the upstream script this file ports.
//! - `backup.c:353` - `rprintf(FINFO, "backed up %s to %s\n", fname, buf)`.
//! - `options.c` - `--backup` / `--backup-dir` / `--suffix` wiring.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;
use test_support::{OcRsyncCliRunner, require_binary};

/// A fixed old timestamp used to backdate destination files so rsync's
/// quick-check (matching size + mtime) never short-circuits the transfer.
fn backdate(path: &Path) {
    // 2000-01-01T00:00:00Z, comfortably older than the freshly written source.
    set_file_mtime(path, FileTime::from_unix_time(946_684_800, 0)).expect("backdate mtime");
}

/// Seed `dest` with `old` content and backdate it, so a later transfer of
/// `new` content triggers an overwrite (and thus a backup).
fn seed_dest(dest: &Path, old: &str) {
    fs::write(dest, old).expect("seed dest");
    backdate(dest);
}

#[test]
fn backup_default_suffix_preserves_prior_version() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    fs::write(from.join("name1"), "new-content\n").expect("write source");
    seed_dest(&to.join("name1"), "old-content\n");

    // Upstream: $RSYNC -ai --info=backup --no-whole-file --backup from/ to/
    let out = OcRsyncCliRunner::new()
        .args(["-ai", "--info=backup", "--no-whole-file", "--backup"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        fs::read_to_string(to.join("name1")).unwrap(),
        "new-content\n",
        "new content must overwrite dest"
    );
    assert_eq!(
        fs::read_to_string(to.join("name1~")).unwrap(),
        "old-content\n",
        "prior content must survive in the ~ backup"
    );
    assert!(
        out.stdout_contains("backed up name1 to name1~"),
        "missing upstream backup message; stdout was:\n{}",
        out.stdout_str()
    );
}

#[test]
fn backup_custom_suffix_names_the_backup() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    fs::write(from.join("name1"), "fresh\n").expect("write source");
    seed_dest(&to.join("name1"), "stale\n");

    let out = OcRsyncCliRunner::new()
        .args([
            "-ai",
            "--info=backup",
            "--no-whole-file",
            "--backup",
            "--suffix=.bak",
        ])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(fs::read_to_string(to.join("name1")).unwrap(), "fresh\n");
    assert_eq!(
        fs::read_to_string(to.join("name1.bak")).unwrap(),
        "stale\n",
        "custom --suffix must name the backup"
    );
    assert!(
        out.stdout_contains("backed up name1 to name1.bak"),
        "missing custom-suffix backup message; stdout was:\n{}",
        out.stdout_str()
    );
}

#[test]
fn backup_dir_relocates_prior_version_without_suffix() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    let bak = base.join("bak");
    fs::create_dir_all(from.join("deep")).expect("mkdir from/deep");
    fs::create_dir_all(&to).expect("mkdir to");

    // Nested source path, mirroring upstream's deep/name1 layout.
    fs::write(from.join("deep/name1"), "v-new\n").expect("write source");
    fs::create_dir_all(to.join("deep")).expect("mkdir to/deep");
    seed_dest(&to.join("deep/name1"), "v-old\n");

    // Upstream: $RSYNC -ai --info=backup --no-whole-file --backup --backup-dir=BAK from/ to/
    let mut backup_dir_arg = std::ffi::OsString::from("--backup-dir=");
    backup_dir_arg.push(&bak);
    let out = OcRsyncCliRunner::new()
        .args([
            std::ffi::OsString::from("-ai"),
            std::ffi::OsString::from("--info=backup"),
            std::ffi::OsString::from("--no-whole-file"),
            std::ffi::OsString::from("--backup"),
            backup_dir_arg,
        ])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        fs::read_to_string(to.join("deep/name1")).unwrap(),
        "v-new\n"
    );
    // No in-place ~ backup when a backup-dir is used.
    assert!(
        !to.join("deep/name1~").exists(),
        "backup-dir mode must not also create an in-place ~ backup"
    );
    // Prior content relocated under the backup dir at the same relative path.
    assert_eq!(
        fs::read_to_string(bak.join("deep/name1")).unwrap(),
        "v-old\n",
        "prior content must land under the backup dir at deep/name1"
    );
    assert!(
        out.stdout_contains("backed up deep/name1 to"),
        "missing backup-dir message; stdout was:\n{}",
        out.stdout_str()
    );
}

/// Trailing-slash form so rsync copies a directory's contents.
fn slash(path: &Path) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    s.push("/");
    s
}
