//! An attribute-only change reports the BARE NAME, not `"is uptodate"`.
//!
//! Upstream decides the wording inside `set_file_attrs()` from its own
//! `updated` bitmask - not from whether the file's DATA moved:
//!
//! ```c
//! /* rsync.c:823-828 */
//! if (INFO_GTE(NAME, 2) && flags & ATTRS_REPORT) {
//!     if (updated)
//!         rprintf(FCLIENT, "%s\n", fname);
//!     else
//!         rprintf(FCLIENT, "%s is uptodate\n", fname);
//! }
//! ```
//!
//! `updated` accumulates `UPDATED_MODE` / `_OWNER` / `_TIMES` / `_ACLS` /
//! `_XATTRS`, so a chmod against a file whose contents already match is
//! "updated, just not transferred" and prints the bare name. oc reported
//! `"a is uptodate"` for that case because it keyed the wording on the event
//! KIND (`MetadataReused`) rather than on whether anything actually changed.
//!
//! Measured against the real rsync 3.5.0 binary on this fixture:
//!
//! | flags            | upstream | oc (pre-fix)    |
//! |------------------|----------|-----------------|
//! | `-a`, `-a -v`    | silent   | silent          |
//! | `-a -vv`         | `a`      | `a is uptodate` |
//! | `-a --info=name2`| `a`      | `a is uptodate` |
//!
//! Both verbosity spellings are exercised deliberately: `--info=name2` reaches
//! the renderer's verbosity-0 branch while `-vv` reaches the verbose branch,
//! and before the fix each carried its own copy of the defect. A test covering
//! only one of them would leave the other site unpinned.
//!
//! `unchanged_file_still_reports_is_uptodate` is the non-vacuity companion: it
//! is the case upstream DOES phrase as `"is uptodate"`, so a build that simply
//! stopped emitting the notice - or emitted it always - fails exactly one of
//! the two tests. Neither test is meaningful without the other.

#![cfg(unix)]

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Builds `src/a` and `dst/a` with identical contents and identical mtimes, so
/// rsync's quick check reports the contents as already up to date. `dst_mode`
/// decides whether anything is left for `set_file_attrs` to change.
///
/// Equal size AND equal mtime is what makes the quick check pass
/// (upstream generator.c `quick_check_ok()`), which is the precondition for
/// reaching the notice under test at all.
fn fixture(root: &Path, dst_mode: u32) -> (PathBuf, PathBuf) {
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");
    fs::write(src.join("a"), b"hello\n").expect("write src/a");
    fs::write(dst.join("a"), b"hello\n").expect("write dst/a");

    fs::set_permissions(src.join("a"), fs::Permissions::from_mode(0o644)).expect("chmod src/a");
    fs::set_permissions(dst.join("a"), fs::Permissions::from_mode(dst_mode)).expect("chmod dst/a");

    // Copy the source mtime onto the destination so the quick check sees an
    // identical (size, mtime) pair. Writing both files "at the same time" is
    // not enough - sub-second timestamps make that a race.
    let modified = fs::metadata(src.join("a"))
        .expect("stat src/a")
        .modified()
        .expect("src mtime");
    File::options()
        .write(true)
        .open(dst.join("a"))
        .expect("open dst/a")
        .set_times(FileTimes::new().set_modified(modified))
        .expect("set dst/a mtime");

    (src, dst)
}

/// Runs one local copy and returns the line the notice under test would occupy,
/// or `None` when nothing was printed for the entry.
fn name_row(root: &Path, dst_mode: u32, verbosity_flag: &str) -> Option<String> {
    let (src, dst) = fixture(root, dst_mode);
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("-a")
        .arg(verbosity_flag)
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("run oc-rsync");
    assert!(
        output.status.success(),
        "transfer failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| *line == "a" || *line == "a is uptodate")
        .map(str::to_owned)
}

/// A chmod against an otherwise-identical file: upstream counts that as
/// `updated`, so the row is the bare name.
///
/// upstream: rsync.c:823-828 `set_file_attrs()`.
#[test]
fn attribute_only_change_reports_the_bare_name() {
    for flag in ["-vv", "--info=name2"] {
        let temp = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            name_row(temp.path(), 0o600, flag).as_deref(),
            Some("a"),
            "an attribute-only change must print the bare name at `{flag}`"
        );
    }
}

/// Nothing changed at all, so `updated` stays zero and the `is uptodate`
/// wording is correct. Without this cell, "never say uptodate" would pass.
///
/// upstream: rsync.c:826 `set_file_attrs()`.
#[test]
fn unchanged_file_still_reports_is_uptodate() {
    for flag in ["-vv", "--info=name2"] {
        let temp = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            name_row(temp.path(), 0o644, flag).as_deref(),
            Some("a is uptodate"),
            "a fully unchanged entry must keep the uptodate wording at `{flag}`"
        );
    }
}
