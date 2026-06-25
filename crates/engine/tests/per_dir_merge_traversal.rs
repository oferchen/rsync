//! Integration tests for per-directory merge (`dir-merge`) traversal in the
//! engine local-copy path, mirroring the LOCAL legs of upstream's
//! `testsuite/exclude.test`.
//!
//! Most cases exercise behaviours that upstream `exclude.c` defines for
//! per-directory merge files during the copy walk; the final case covers the
//! `--delete-before` deletion pass, where the destination-side merge load must
//! stay isolated from the source-side filter evaluation.
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
//! - `delete.c:65-115 delete_in_dir` - the destination deletion scan pushes the
//!   destination's per-dir filters onto a separate chain and pops them
//!   afterwards, never perturbing the sender's filter list.

use std::fs;
use std::path::Path;

use engine::local_copy::{
    DeleteTiming, DirMergeOptions, DirMergeRule, FilterProgram, FilterProgramEntry,
    LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
};
use filters::FilterRule;
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

    // Trailing slash so the source contents copy into `dest` directly. Without
    // it, upstream get_local_name keeps the source name (`dest/<source>/...`);
    // these tests assert on the per-directory filter decisions, not the layout,
    // so copying the contents keeps every assertion path stable.
    let mut source_arg = source.to_path_buf().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_arg, dest.to_path_buf().into_os_string()];
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

/// Regression: a populated destination plus a sender-side per-directory
/// `dir-merge .filt` under `--delete-before` must NOT resurrect files that the
/// merge rules exclude from the sender flist.
///
/// The destination is a full copy of the source, so every excluded file
/// pre-exists. With a sender-side merge, `bar/.filt`, `foo/file2.old`, and the
/// nested `bar/down/to/foo/file1.bak` are absent from the sender flist and must
/// be removed by `--delete-before`, exactly as upstream does and exactly as the
/// `--delete-during` timing already did. A `protect nodel.deep` rule must keep
/// `nodel.deep` in every cell (the data-loss guardrail).
///
/// Before the fix, the destination-deletion scan loaded the destination's own
/// `.filt` files onto the shared dynamic merge stack; under `--delete-before`
/// (deletion runs ahead of the child source plans) that leaked frame corrupted
/// the source-side `allows()` evaluation, so the `- .filt`/`- *.old`/`- *.bak`
/// rules stopped matching and the excluded files were copied back and survived.
///
/// upstream: delete.c:65-115 delete_in_dir runs the destination per-dir filter
/// push/pop against a separate delete_filt chain, isolating it from the sender
/// filter_list; exclude.c:801 push_local_filters keeps inherited rules in
/// lp->head, so a destination merge file may register nested directives an
/// arbitrary depth deep that a per-index pop cannot balance.
#[test]
fn delete_before_does_not_resurrect_sender_excluded_files_into_populated_dest() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");

    // Source tree mirrors the contested legs of upstream exclude.test.
    fs::create_dir_all(source.join("foo/sub")).expect("mkdir foo/sub");
    fs::create_dir_all(source.join("bar/down/to/foo")).expect("mkdir bar/down/to/foo");
    fs::create_dir_all(source.join("bar/down/to/bar/baz")).expect("mkdir bar/.../baz");

    // Root .filt mirrors upstream exclude.test: it registers a nested
    // `: .filt-temp` dir-merge directive BEFORE the static excludes. That nested
    // directive is what makes the leak observable - when the destination scan
    // loads the destination's own root .filt, the nested registration lands an
    // extra index deep that the per-index pop cannot balance, corrupting the
    // source-side allows() for the child directories under --delete-before.
    fs::write(
        source.join(".filt"),
        ": .filt-temp\nclear\n- .filt\n- *.bak\n- *.old\n",
    )
    .expect("write .filt");
    // bar/.filt registers a nested dir-merge .filt2 (a second growing-list
    // directive whose loaded segments deepen the imbalance).
    fs::write(source.join("bar/.filt"), "dir-merge .filt2\n").expect("write bar/.filt");
    fs::write(source.join("bar/down/to/bar/.filt2"), "- *.deep\n").expect("write .filt2");

    // Files that the merge rules exclude from the SENDER flist; each must be
    // deleted from the populated destination by --delete-before.
    fs::write(source.join("foo/file1"), b"kept").expect("write foo/file1");
    fs::write(source.join("foo/file2.old"), b"excluded-old").expect("write file2.old");
    fs::write(source.join("bar/down/to/foo/file1"), b"kept").expect("write keeper");
    fs::write(source.join("bar/down/to/foo/file1.bak"), b"excluded-bak").expect("write .bak");
    // *.deep is excluded from transfer but protected from deletion via `P`.
    fs::write(source.join("bar/down/to/bar/baz/file5.deep"), b"deep").expect("write deep");

    // Full copy: every file (including the to-be-excluded ones) pre-exists.
    copy_tree(&source, &dest);
    // nodel.deep exists ONLY in the destination and is protected by `protect`.
    fs::write(dest.join("bar/down/to/bar/baz/nodel.deep"), b"retained").expect("write nodel");

    let program = FilterProgram::new([
        FilterProgramEntry::DirMerge(DirMergeRule::new(
            ".filt",
            DirMergeOptions::new().sender_modifier(),
        )),
        FilterProgramEntry::Rule(FilterRule::protect("nodel.deep")),
    ])
    .expect("filter program compiles");

    // Trailing slash on the source: copy its CONTENTS into dest (rsync
    // `src/ dst/`), so the merge rules and populated destination line up at the
    // same relative roots, exactly as the upstream exclude.test leg does.
    let mut source_contents = source.clone().into_os_string();
    source_contents.push("/");
    let operands = vec![source_contents, dest.to_path_buf().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::new()
        .recursive(true)
        .delete(true)
        .delete_before(true)
        .with_filter_program(Some(program));
    assert_eq!(options.delete_timing(), Some(DeleteTiming::Before));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    // The sender-excluded files must be deleted, never resurrected.
    for resurrected in ["bar/.filt", "foo/file2.old", "bar/down/to/foo/file1.bak"] {
        assert!(
            !dest.join(resurrected).exists(),
            "{resurrected} is excluded from the sender flist and must be deleted \
             by --delete-before, not resurrected from the populated destination",
        );
    }

    // The data-loss guardrail: protected and non-excluded files survive.
    assert!(
        dest.join("bar/down/to/bar/baz/nodel.deep").exists(),
        "`protect nodel.deep` must keep nodel.deep in the delete-before cell",
    );
    assert!(
        dest.join("foo/file1").exists(),
        "non-excluded foo/file1 must remain",
    );
    assert!(
        dest.join("bar/down/to/foo/file1").exists(),
        "non-excluded bar/down/to/foo/file1 must remain",
    );
}

/// Recursively copies a directory tree, mirroring `cp -a` for the populated
/// destination used by the deletion regression test.
fn copy_tree(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create dest dir");
    for entry in fs::read_dir(source).expect("read source dir") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}
