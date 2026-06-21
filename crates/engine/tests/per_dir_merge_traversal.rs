//! Integration tests for per-directory merge (`dir-merge`) traversal in the
//! engine local-copy path, mirroring the LOCAL legs of upstream's
//! `testsuite/exclude.test`.
//!
//! These exercise three behaviours that upstream `exclude.c` defines for
//! per-directory merge files and that must hold during the copy walk - not the
//! deletion pass. The files under test enter the transfer purely on the merge
//! rules; no `--delete` is involved.
//!
//! # Upstream Reference
//!
//! - `exclude.c:200-207 add_rule` - an anchored pattern loaded from a per-dir
//!   merge file is rooted at the merge file's directory, not the transfer root.
//! - `exclude.c:1248-1255 parse_rule_tok` - the `:C` modifier registers a
//!   `.cvsignore` per-directory merge.
//! - `exclude.c:787-789 push_local_filters` - the push loop walks the GROWING
//!   `mergelist_cnt`, so a `dir-merge` directive registered while loading a
//!   directory's merge file is itself loaded against that same directory.

use std::fs;
use std::path::Path;

use engine::local_copy::{
    DirMergeOptions, DirMergeRule, FilterProgram, FilterProgramEntry, LocalCopyExecution,
    LocalCopyOptions, LocalCopyPlan,
};
use tempfile::tempdir;

/// Runs a recursive local copy from `source` to `dest` using a filter program
/// that registers `dir-merge .filt` (the per-directory merge filename used
/// throughout upstream `exclude.test`).
fn copy_with_dir_merge(source: &Path, dest: &Path) {
    let program = FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
        ".filt",
        DirMergeOptions::default(),
    ))])
    .expect("filter program compiles");

    let operands = vec![
        source.to_path_buf().into_os_string(),
        dest.to_path_buf().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::new()
        .recursive(true)
        .with_filter_program(Some(program));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");
}

/// `- /file1` inside `foo/.filt` is anchored to the merge file's directory, so
/// it must exclude `foo/file1` while leaving `foo/sub/file1` (a deeper path
/// that the anchor does not reach) transferred.
///
/// upstream: exclude.c:200-207 add_rule prepends `dirbuf + module_dirlen` to a
/// leading-`/` pattern, turning `- /file1` in `foo/.filt` into an anchored
/// `foo/file1` rule. Without that prefix the anchor would (wrongly) bind to the
/// transfer root and match nothing, leaking `foo/file1` into the destination.
#[test]
fn anchored_rule_in_per_dir_merge_anchors_at_merge_file_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("foo/sub")).expect("mkdir");

    fs::write(source.join("foo/.filt"), "- /file1\n").expect("write .filt");
    fs::write(source.join("foo/file1"), b"excluded").expect("write file1");
    fs::write(source.join("foo/sub/file1"), b"kept").expect("write sub/file1");

    copy_with_dir_merge(&source, &dest);

    assert!(
        !dest.join("foo/file1").exists(),
        "`- /file1` in foo/.filt must exclude the anchored foo/file1",
    );
    assert!(
        dest.join("foo/sub/file1").exists(),
        "the anchor must not reach the deeper foo/sub/file1",
    );
}

/// `:C` inside `mid/.filt` registers `.cvsignore` as a per-directory CVS-ignore
/// source that applies to the directory containing the merge file. The
/// `mid/.cvsignore` listing `one-in-one-out` must therefore exclude
/// `mid/one-in-one-out`.
///
/// upstream: exclude.c:1248-1255 - the `C` modifier on a `:` (dir-merge) rule
/// sets `FILTRULE_CVS_IGNORE` with the `.cvsignore` default; exclude.c:787-789
/// then loads that registration against the current directory (not only
/// subdirectories) because the push loop walks the growing mergelist count.
#[test]
fn cvs_modifier_in_per_dir_merge_loads_cvsignore_for_current_dir() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("mid")).expect("mkdir");

    fs::write(source.join("mid/.filt"), ":C\n").expect("write .filt");
    fs::write(source.join("mid/.cvsignore"), "one-in-one-out\n").expect("write .cvsignore");
    fs::write(source.join("mid/one-in-one-out"), b"excluded").expect("write target");
    fs::write(source.join("mid/one-for-all"), b"kept").expect("write keeper");

    copy_with_dir_merge(&source, &dest);

    assert!(
        !dest.join("mid/one-in-one-out").exists(),
        ":C in mid/.filt must load mid/.cvsignore and exclude one-in-one-out",
    );
    assert!(
        dest.join("mid/one-for-all").exists(),
        "files not listed in .cvsignore must still transfer",
    );
}

/// A `dir-merge .filt2` registered inside `bar/.filt` must take effect in the
/// directory that holds the registering file and all of its descendants. The
/// `bar/.filt2` rule `- *.deep` must therefore exclude `bar/baz/file5.deep`,
/// which lives in a sub-subdirectory below the registration point.
///
/// upstream: exclude.c:787-789 push_local_filters - registering a per-dir merge
/// while loading `bar/.filt` grows `mergelist_cnt`, so `bar/.filt2` is loaded
/// for `bar/` itself; exclude.c:801 sets `lp->tail = NULL`, keeping the loaded
/// rule in `lp->head` so it continues matching descendants like
/// `bar/baz/file5.deep`.
#[test]
fn same_dir_nested_dir_merge_applies_to_current_dir_and_descendants() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(source.join("bar/baz")).expect("mkdir");

    fs::write(source.join("bar/.filt"), "dir-merge .filt2\n").expect("write .filt");
    fs::write(source.join("bar/.filt2"), "- *.deep\n").expect("write .filt2");
    fs::write(source.join("bar/baz/file5.deep"), b"excluded").expect("write deep");
    fs::write(source.join("bar/baz/keep.txt"), b"kept").expect("write keep");

    copy_with_dir_merge(&source, &dest);

    assert!(
        !dest.join("bar/baz/file5.deep").exists(),
        "bar/.filt2's `- *.deep` (registered by bar/.filt) must exclude the \
         descendant bar/baz/file5.deep",
    );
    assert!(
        dest.join("bar/baz/keep.txt").exists(),
        "non-matching descendants must still transfer",
    );
}
