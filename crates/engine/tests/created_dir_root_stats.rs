//! Regression coverage for the `--stats` "created dirs" tally of the
//! destination root on a fresh, non-relative transfer.
//!
//! Upstream `main.c:802-808` (`get_local_name`) pre-flight-mkdirs the
//! destination root and unconditionally prints `created directory <dest>`, but
//! only flags the flist top entry `FLAG_DIR_CREATED` - and thus counts the root
//! toward `stats.created_dirs` (receiver.c:733-738) - when that entry's basename
//! is `"."`:
//!
//! ```c
//! if (flist->high >= flist->low
//!  && strcmp(flist->files[flist->low]->basename, ".") == 0)
//!     flist->files[0]->flags |= FLAG_DIR_CREATED;
//! ```
//!
//! The top entry is `"."` only for a copy-contents transfer (a trailing-slash or
//! dot-dir source whose implied root maps onto the destination). A single-file
//! source (`src/a.txt`) or a no-trailing-slash directory source (`src`) has a
//! NAMED top entry (`a.txt` / `src`), so upstream emits the notice yet never
//! counts the destination root itself; the named entries are counted on their
//! own. These tests pin that distinction so oc does not over-count the root.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

/// Builds a source tree
///
/// ```text
/// src/
///   a.txt
///   sub1/b.txt
///   sub2/c.txt
/// ```
///
/// and returns `(src_dir, dest_root, tempdir)`. `dest_root` does NOT exist yet,
/// so every transfer below runs against a fresh destination.
fn source_tree() -> (PathBuf, PathBuf, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(src.join("sub1")).expect("sub1");
    fs::create_dir_all(src.join("sub2")).expect("sub2");
    fs::write(src.join("a.txt"), b"a").expect("a.txt");
    fs::write(src.join("sub1").join("b.txt"), b"b").expect("b.txt");
    fs::write(src.join("sub2").join("c.txt"), b"c").expect("c.txt");
    let dest = temp.path().join("dst");
    (src, dest, temp)
}

fn recursive_options() -> LocalCopyOptions {
    LocalCopyOptions::default()
        .recursive(true)
        .collect_events(true)
}

fn created_dirs(operands: &[OsString], dest: &Path) -> u64 {
    let plan = LocalCopyPlan::from_operands(operands).expect("plan");
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, recursive_options())
        .expect("copy succeeds");
    assert!(dest.exists(), "destination root materialised");
    report.summary().directories_created()
}

/// `-a src/a.txt dst/` (single file, non-relative, fresh dest): upstream mkdirs
/// and announces `dst` but the flist top entry is the file `a.txt`, not `"."`,
/// so `FLAG_DIR_CREATED` is never set and the created-dir tally stays 0. oc must
/// not inflate it by counting the pre-flight mkdir of the destination root.
#[test]
fn single_file_non_relative_does_not_count_destination_root() {
    let (src, dest, _temp) = source_tree();
    let operands = vec![
        src.join("a.txt").into_os_string(),
        format!("{}/", dest.display()).into(),
    ];
    assert_eq!(
        created_dirs(&operands, &dest),
        0,
        "a named single-file top entry never sets FLAG_DIR_CREATED on the root"
    );
    assert!(dest.join("a.txt").is_file());
}

/// `-a src dst/` (directory, no trailing slash, non-relative, fresh dest): the
/// flist top entry is the NAMED directory `src`, not `"."`, so upstream counts
/// `src` and its two subdirs (3) but NOT the pre-flight `dst` root. Before the
/// fix oc reported 4 (the extra being the wrongly-counted `dst` root).
#[test]
fn no_trailing_slash_dir_does_not_count_destination_root() {
    let (src, dest, _temp) = source_tree();
    let operands = vec![
        src.clone().into_os_string(),
        format!("{}/", dest.display()).into(),
    ];
    assert_eq!(
        created_dirs(&operands, &dest),
        3,
        "counts src + sub1 + sub2; the dst root is announced but not counted"
    );
    assert!(dest.join("src").join("sub1").join("b.txt").is_file());
}

/// Control: `-a src/ dst/` (copy-contents, non-relative, fresh dest). Here the
/// flist top entry IS the synthetic `"."` root that maps onto `dst`, so upstream
/// DOES set `FLAG_DIR_CREATED` and counts the root alongside the two subdirs
/// (3). This is the case the fix must leave unchanged - the root count is
/// legitimate only when the top entry is `"."`.
#[test]
fn copy_contents_still_counts_destination_root() {
    let (src, dest, _temp) = source_tree();
    let operands = vec![
        format!("{}/", src.display()).into(),
        format!("{}/", dest.display()).into(),
    ];
    assert_eq!(
        created_dirs(&operands, &dest),
        3,
        "the \".\" top entry maps to dst, so the root counts with sub1 + sub2"
    );
    assert!(dest.join("sub1").join("b.txt").is_file());
}
