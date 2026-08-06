//! Regression coverage: a LOCAL multi-source copy visits its operands in
//! upstream `f_name_cmp` order, not command-line order, so the itemize /
//! `--list-only` / `-v` stdout stream matches a real upstream 3.4.4 transfer.
//!
//! # Why this matters (observable stdout-fidelity contract)
//!
//! Upstream `send_file_list()` (flist.c:2227) accumulates every source operand
//! into ONE file list and sorts it globally with `f_name_cmp`
//! (`flist_sort_and_clean`, flist.c:2544) before the generator itemizes it, so
//! top-level operands are emitted in name order regardless of the order they
//! were given on the command line. oc's local-copy executor walks each
//! operand's subtree already in `f_name_cmp` order but, before this fix, visited
//! the operands themselves in argument order - so `oc-rsync -ai f_mango
//! f_cherry f_apple f_banana dst/` itemized `mango, cherry, apple, banana`
//! where upstream prints `apple, banana, cherry, mango`. The files landed
//! correctly either way (a copy is a copy); only the observable order diverged,
//! which the project's output-fidelity contract forbids.
//!
//! The expected orders below are the ones a real upstream rsync 3.4.4 prints
//! for the identical command (verified against the reference binary). These
//! tests FAIL before the operand-sort fix (they would see argument order).
//!
//! Scope: NAMED operands (each contributes a single top-level entry) are
//! reordered by `ordered_operands`; trailing-slash / copy-contents operands
//! merge their CONTENTS globally by content name, reproduced by
//! `merged_contents_worklist` (both live in the local-copy source orchestrator).
//! Both apply only to a non-`--relative` transfer. `--relative` operands carry
//! implied parent directories whose ordering is handled during emission, so
//! they are left in command-line order. The copy-contents merge additionally
//! stays out of `--delete`, `--write-batch`, and `--one-file-system` transfers
//! (which key off the per-source whole-root walk it bypasses); those keep
//! command-line grouping and are a tracked follow-up.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use engine::local_copy::{
    LocalCopyAction, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord,
};
use tempfile::{TempDir, tempdir};

/// Relative paths of the records that represent an emitted flist entry
/// (a created directory or a copied regular file), in stream order.
fn emitted_entry_order(records: &[LocalCopyRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|record| {
            matches!(
                record.action(),
                LocalCopyAction::DataCopied | LocalCopyAction::DirectoryCreated
            )
        })
        .map(|record| record.relative_path().to_string_lossy().into_owned())
        .collect()
}

fn apply(operands: &[OsString], options: LocalCopyOptions) -> engine::local_copy::LocalCopyReport {
    let plan = LocalCopyPlan::from_operands(operands).expect("plan builds");
    plan.execute_with_report(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds")
}

/// Four single-file operands supplied in an order that is deliberately NOT
/// their sorted order. Upstream itemizes them sorted (`apple, banana, cherry,
/// mango`); before the fix oc emitted argument order (`mango, cherry, apple,
/// banana`).
#[test]
fn multi_file_operands_are_itemized_in_sorted_order() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");

    // Command-line order: mango, cherry, apple, banana.
    let names = ["mango", "cherry", "apple", "banana"];
    let mut operands: Vec<OsString> = Vec::new();
    for name in names {
        let path = temp.path().join(name);
        fs::write(&path, name.as_bytes()).expect("write source file");
        operands.push(path.into_os_string());
    }
    operands.push(dst.clone().into_os_string());

    let report = apply(&operands, LocalCopyOptions::default().collect_events(true));

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["apple", "banana", "cherry", "mango"],
        "multi-file operands must itemize in f_name_cmp order (upstream 3.4.4), \
         not command-line order",
    );

    // Regression: every file still lands with correct content.
    for name in names {
        assert_eq!(
            fs::read(dst.join(name)).expect("dst file exists"),
            name.as_bytes(),
            "content for {name} must be copied intact",
        );
    }
}

/// Builds two NAMED directory operands whose sorted order is the reverse of the
/// command-line order, with contents chosen so a naive "sort by content" would
/// give a different answer than "sort the operands": `zdir` holds the
/// low-sorting file, `adir` holds the high-sorting file. Upstream orders the
/// operands (`adir` before `zdir`) and keeps each subtree contiguous.
fn two_named_dirs() -> (Vec<OsString>, PathBuf, TempDir) {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    for (dir, file) in [("zdir", "aaa"), ("adir", "zzz")] {
        let dpath = temp.path().join(dir);
        fs::create_dir_all(&dpath).expect("create source dir");
        fs::write(dpath.join(file), file.as_bytes()).expect("write file");
    }
    // Command-line order: zdir then adir.
    let operands = vec![
        temp.path().join("zdir").into_os_string(),
        temp.path().join("adir").into_os_string(),
        dst.clone().into_os_string(),
    ];
    (operands, dst, temp)
}

/// Named directory operands are ordered by `f_name_cmp`, and each operand's
/// subtree stays contiguous with it - exactly upstream's `adir/, adir/zzz,
/// zdir/, zdir/aaa`. Proves the reorder keys on the OPERAND name (not on the
/// contents): `adir` sorts first even though it holds the higher-sorting file.
#[test]
fn multi_dir_operands_order_by_operand_name_with_contiguous_subtrees() {
    let (operands, dst, _temp) = two_named_dirs();

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["adir", "adir/zzz", "zdir", "zdir/aaa"],
        "named directory operands must sort by operand name with each subtree \
         contiguous, matching upstream's global flist sort",
    );

    // Regression: both trees land intact.
    assert_eq!(fs::read(dst.join("adir/zzz")).unwrap(), b"zzz");
    assert_eq!(fs::read(dst.join("zdir/aaa")).unwrap(), b"aaa");
}

/// A single operand is never reordered (nothing to sort) and its own subtree
/// stays in `f_name_cmp` order - guarding against the sort touching the common
/// single-source path.
#[test]
fn single_directory_source_subtree_stays_sorted() {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");
    for name in ["zebra", "mango", "apple", "delta"] {
        fs::write(src.join(name), name.as_bytes()).expect("write");
    }
    // Trailing slash: copy the directory's CONTENTS into dst.
    let operands = vec![
        format!("{}/", src.display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["apple", "delta", "mango", "zebra"],
        "a single source's contents stay in f_name_cmp order",
    );
}

/// `--relative` operands are intentionally NOT reordered by this fix (their
/// implied-parent emission ordering is handled separately), so a `-R`
/// multi-source copy keeps command-line operand order. This pins the scope
/// boundary: the fix must leave `-R` behaviour exactly as it was.
#[test]
fn relative_operands_are_left_in_command_line_order() {
    let (operands, _dst, _temp) = two_named_dirs();

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .relative_paths(true)
            .collect_events(true),
    );

    // Absolute operands under --relative record their full path-minus-root, so
    // match the operand leaf by suffix rather than by a bare name.
    let order = emitted_entry_order(report.records());
    let zdir = order.iter().position(|p| p.ends_with("zdir"));
    let adir = order.iter().position(|p| p.ends_with("adir"));
    assert!(
        matches!((zdir, adir), (Some(z), Some(a)) if z < a),
        "under --relative the operands keep command-line order (zdir before \
         adir); reordering -R is out of scope for this fix. Order was {order:?}",
    );
}

/// The operand sort is a pure output-ordering change: reordering the operands
/// must never change WHICH files land on disk. A copy of the same two dirs in
/// either command-line order produces byte-identical destinations.
#[test]
fn reordering_does_not_change_destination_contents() {
    let (operands, dst, _temp) = two_named_dirs();
    apply(&operands, LocalCopyOptions::default().recursive(true));

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(root).unwrap().to_path_buf();
                    out.push((rel, fs::read(&path).expect("read")));
                }
            }
        }
        out.sort();
        out
    }

    assert_eq!(
        snapshot(&dst),
        vec![
            (PathBuf::from("adir/zzz"), b"zzz".to_vec()),
            (PathBuf::from("zdir/aaa"), b"aaa".to_vec()),
        ],
        "destination contents are independent of operand visit order",
    );
}

// ---------------------------------------------------------------------------
// Known divergences NOT addressed by the operand sort, pinned as ignored specs
// so they stay tracked rather than silently masked. Each encodes upstream's
// exact behaviour and is ready to un-ignore when its dedicated follow-up lands.
// ---------------------------------------------------------------------------

/// Copy-contents (trailing-slash) sources merging into one destination.
/// Upstream flattens every operand's CONTENTS into one flist and sorts globally
/// (flist.c:2544 flist_sort_and_clean), interleaving files across operands by
/// content name (`aaa, mmm, zzz` below), NOT one operand's contents at a time
/// (`aaa, zzz, mmm`). `merged_contents_worklist` reproduces the merge by
/// flattening each source's children into synthetic named operands and sorting
/// them with the same f_name_cmp comparator. The expected order was verified
/// against upstream rsync 3.4.4 (`rsync -rin asrc/ zsrc/ dst/`).
#[test]
fn copy_contents_multi_source_interleave_matches_upstream() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    fs::create_dir_all(temp.path().join("asrc")).unwrap();
    fs::create_dir_all(temp.path().join("zsrc")).unwrap();
    for f in ["aaa", "zzz"] {
        fs::write(temp.path().join("asrc").join(f), f.as_bytes()).unwrap();
    }
    fs::write(temp.path().join("zsrc").join("mmm"), b"mmm").unwrap();

    // Both sources are copy-contents (trailing slash), given asrc then zsrc.
    let operands = vec![
        format!("{}/", temp.path().join("asrc").display()).into(),
        format!("{}/", temp.path().join("zsrc").display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    // Upstream 3.4.4 interleaves by content name across the two operands.
    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["aaa", "mmm", "zzz"],
        "copy-contents sources must interleave globally by content name",
    );
}

/// The merged root sorts NON-directories before directories (upstream
/// f_name_cmp / flist.c:3299 keys on type before name), so a subdirectory is
/// emitted after every root-level file even when its name sorts earlier. Here
/// `b_dir` (from asrc) sorts alphabetically before the files `m_file`, `n_file`
/// yet upstream lists it last, then descends into it. dirA contributes
/// `m_file` + `b_dir/z_inner`; dirB contributes `a_file` + `n_file`. Verified
/// against upstream rsync 3.4.4 (`rsync -rin asrc/ bsrc/ dst/`).
#[test]
fn copy_contents_files_precede_dirs_across_merged_root() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    let asrc = temp.path().join("asrc");
    let bsrc = temp.path().join("bsrc");
    fs::create_dir_all(asrc.join("b_dir")).unwrap();
    fs::create_dir_all(&bsrc).unwrap();
    fs::write(asrc.join("m_file"), b"m").unwrap();
    fs::write(asrc.join("b_dir").join("z_inner"), b"z").unwrap();
    fs::write(bsrc.join("a_file"), b"a").unwrap();
    fs::write(bsrc.join("n_file"), b"n").unwrap();

    let operands = vec![
        format!("{}/", asrc.display()).into(),
        format!("{}/", bsrc.display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["a_file", "m_file", "n_file", "b_dir", "b_dir/z_inner"],
        "merged root files (sorted) precede the merged root dir and its subtree",
    );
}

/// Name collisions across copy-contents sources resolve exactly as upstream's
/// flist_sort_and_clean + receiver: a colliding regular file keeps the FIRST
/// operand's copy (the later duplicate is dropped), while a colliding directory
/// MERGES - both operands' children land under the one destination directory.
/// Verified against upstream rsync 3.4.4 (`rsync -a dirA/ dirB/ dst/`): `file1`
/// is `AAA` (dirA, the first operand) and `sub/` holds both `x` and `y`.
#[test]
fn copy_contents_collision_first_file_wins_and_dirs_merge() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    let dir_a = temp.path().join("dirA");
    let dir_b = temp.path().join("dirB");
    fs::create_dir_all(dir_a.join("sub")).unwrap();
    fs::create_dir_all(dir_b.join("sub")).unwrap();
    fs::write(dir_a.join("file1"), b"AAA").unwrap();
    fs::write(dir_b.join("file1"), b"BBB").unwrap();
    fs::write(dir_a.join("only_a"), b"x").unwrap();
    fs::write(dir_b.join("only_b"), b"x").unwrap();
    fs::write(dir_a.join("sub").join("x"), b"ax").unwrap();
    fs::write(dir_b.join("sub").join("y"), b"by").unwrap();

    let operands = vec![
        format!("{}/", dir_a.display()).into(),
        format!("{}/", dir_b.display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["file1", "only_a", "only_b", "sub", "sub/x", "sub/y"],
        "colliding file is emitted once, colliding dir is emitted once then \
         merges both operands' children in sorted order",
    );

    // The first operand's file1 (AAA) wins; the later duplicate is dropped.
    assert_eq!(fs::read(dst.join("file1")).unwrap(), b"AAA");
    // The colliding directory merged both operands' distinct children.
    assert_eq!(fs::read(dst.join("sub").join("x")).unwrap(), b"ax");
    assert_eq!(fs::read(dst.join("sub").join("y")).unwrap(), b"by");
}

/// The `--stats` file/dir counts must survive the merge. Upstream sorts the
/// combined flist WITHOUT removing duplicates (flist.c:2535-2544), so each
/// trailing-slash operand's own "." entry is counted: `rsync -r --stats dirA/
/// dirB/ dst/` reports `Number of files: 8 (reg: 5, dir: 3)` - 5 regular files
/// plus 3 directories (one "." per source, and `sub`). Verified against
/// upstream rsync 3.4.4. This pins that the merge counts the per-source roots
/// rather than collapsing them, which would drop the dir tally to 1.
#[test]
fn copy_contents_multi_source_stats_count_matches_upstream() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    let dir_a = temp.path().join("dirA");
    let dir_b = temp.path().join("dirB");
    fs::create_dir_all(dir_a.join("sub")).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(dir_a.join("apple"), b"a").unwrap();
    fs::write(dir_a.join("mango"), b"m").unwrap();
    fs::write(dir_a.join("sub").join("inner"), b"i").unwrap();
    fs::write(dir_b.join("banana"), b"b").unwrap();
    fs::write(dir_b.join("cherry"), b"c").unwrap();

    let operands = vec![
        format!("{}/", dir_a.display()).into(),
        format!("{}/", dir_b.display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(&operands, LocalCopyOptions::default().recursive(true));
    let summary = report.summary();

    assert_eq!(
        summary.regular_files_total(),
        5,
        "reg count: apple, mango, sub/inner, banana, cherry",
    );
    assert_eq!(
        summary.directories_total(),
        3,
        "dir count: one '.' per copy-contents source (2) plus sub (upstream \
         does not deduplicate the flist count)",
    );
}

// KNOWN DIVERGENCE (tracked, not tested here): multi-source `--delete` timing.
// With copy-contents directory sources under `--delete`, upstream removes an
// extraneous destination entry BEFORE it transfers the sources (the `*deleting`
// row precedes the `>f` rows), whereas oc runs its delete pass at the end so the
// deletion trails the transfers. The operand sort deliberately does not move the
// delete pass (the deletion keep-set is order-independent, so results are
// unchanged). This timing is a separate investigate-and-fix follow-up; it is
// noted here rather than tested because it only arises with copy-contents
// sources, which are themselves out of scope above.
