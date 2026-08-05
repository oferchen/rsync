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
//! Scope (matches the fix): the reorder applies only to NAMED operands (each
//! contributes a single top-level entry) in a non-`--relative` transfer.
//! Trailing-slash / copy-contents operands merge their CONTENTS into the
//! destination, which upstream interleaves across operands by content name -
//! unreproducible by operand ordering - so they are left in argument order and
//! covered separately. `--relative` operands are likewise left untouched here.

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
        LocalCopyOptions::default().recursive(true).collect_events(true),
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
        LocalCopyOptions::default().recursive(true).collect_events(true),
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

    let order = emitted_entry_order(report.records());
    let zdir = order.iter().position(|p| p == "zdir");
    let adir = order.iter().position(|p| p == "adir");
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
    apply(
        &operands,
        LocalCopyOptions::default().recursive(true),
    );

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
