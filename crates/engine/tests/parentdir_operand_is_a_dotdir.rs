//! A source operand whose last component is `..` is a DOTDIR operand.
//!
//! Upstream never stats such an operand as typed. It APPENDS `/.` to it and
//! records `DOTDIR_NAME` (`flist.c:2595-2602`), which makes the non-relative
//! split at `flist.c:2610-2621` cut `src/../.` at its LAST `/`: `dir` becomes
//! `src/..`, `fn` becomes `.`, and the parent directory's CONTENTS are
//! transferred - exactly what a trailing `/` does.
//!
//! oc used `Path::file_name()` on the operand instead. That returns `None` for
//! a `ParentDir` final component, and the `None` became
//! `LocalCopyArgumentError::DirectoryNameUnavailable` - "cannot determine
//! directory name", exit 23, nothing transferred. MEASURED against rsync 3.5.0:
//! `src/..`, `src/./..`, `src/sym-to-dir/..` and a bare `..` all aborted at 23
//! where upstream exits 0 having copied the parent's contents.
//!
//! # Scope: NOT under `--relative`
//!
//! The rule sits in the arm `--relative` short-circuits past
//! (`flist.c:2581-2583`); a `..` in the ACTIVE part of a `--relative` operand is
//! rejected instead, at `flist.c:2658-2667`. `parent_dir_rule_is_scoped_to_non_relative`
//! pins that boundary.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/flist.c:2595-2602` - the `/.` append and `DOTDIR_NAME`.
//! - `rsync-3.5.0/flist.c:2581-2583` - the `--relative` short-circuit above it.
//! - `rsync-3.5.0/flist.c:2610-2621` - the non-relative last-`/` split.
//! - `rsync-3.5.0/flist.c:2658-2667` - the `--relative` `..` rejection.
//! - `rsync-3.5.0/flist.c:115-118` - `NORMAL_NAME` / `DOTDIR_NAME`.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::{TempDir, tempdir};

/// Plants, under `<temp>/base`:
///
/// ```text
/// base/from/{a.txt, sub/b.txt, sym-to-dir -> ../other/dir_target}
/// base/sibling.txt
/// base/other/dir_target/c.txt
/// ```
///
/// `base/from/..` is `base`; `base/from/sym-to-dir/..` is `base/other`, which
/// the kernel resolves THROUGH the symlink - so the two operands under test name
/// different directories and cannot be satisfied by one hard-coded answer.
///
/// The destination is `<temp>/to`, a sibling of `base`, so copying `base`'s
/// contents can never re-enter the destination.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let temp = tempdir().expect("tempdir");
    let base = temp.path().join("base");
    let to = temp.path().join("to");
    fs::create_dir_all(base.join("from/sub")).expect("mkdir from/sub");
    fs::create_dir_all(base.join("other/dir_target")).expect("mkdir other/dir_target");
    fs::create_dir_all(&to).expect("mkdir to");
    fs::write(base.join("from/a.txt"), b"AAA").expect("write a.txt");
    fs::write(base.join("from/sub/b.txt"), b"BBB").expect("write b.txt");
    symlink("../other/dir_target", base.join("from/sym-to-dir")).expect("plant sym-to-dir");
    fs::write(base.join("sibling.txt"), b"SSS").expect("write sibling.txt");
    fs::write(base.join("other/dir_target/c.txt"), b"CCC").expect("write c.txt");
    (temp, base, to)
}

/// A sorted snapshot of everything under `root`: relative path, entry kind, and
/// the bytes (or link target) that distinguish one entry from another.
///
/// Asserting on this rather than on an exit code is deliberate - a wrong
/// operand rule can exit 0 and still deliver the wrong tree.
fn snapshot(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(root).expect("under root").display();
            let meta = fs::symlink_metadata(&path).expect("symlink_metadata");
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("read_link");
                out.push(format!("{rel} -> {}", target.display()));
            } else if meta.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &path, out);
            } else {
                let bytes = fs::read(&path).expect("read file");
                out.push(format!("{rel} = {}", String::from_utf8_lossy(&bytes)));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

fn options() -> LocalCopyOptions {
    LocalCopyOptions::default().recursive(true).links(true)
}

/// Runs `operand -> to` through the local-copy executor and snapshots `to`.
fn run(operand: &Path, to: &Path, options: LocalCopyOptions) -> Vec<String> {
    let operands = vec![
        OsString::from(operand.as_os_str()),
        OsString::from(to.as_os_str()),
    ];
    let plan =
        LocalCopyPlan::from_operands_with_relative(&operands, options.relative_paths_enabled())
            .expect("plan");
    plan.execute_with_report(LocalCopyExecution::Apply, options)
        .expect("a trailing `..` operand must not abort the transfer");
    snapshot(to)
}

/// `from/..` transfers the CONTENTS of `from`'s parent, not `from` and not a
/// directory literally named `..`.
#[test]
fn parent_dir_operand_transfers_the_parents_contents() {
    let (_temp, base, to) = fixture();

    let got = run(&base.join("from/.."), &to, options());

    assert_eq!(
        got,
        vec![
            "from/".to_owned(),
            "from/a.txt = AAA".to_owned(),
            "from/sub/".to_owned(),
            "from/sub/b.txt = BBB".to_owned(),
            "from/sym-to-dir -> ../other/dir_target".to_owned(),
            "other/".to_owned(),
            "other/dir_target/".to_owned(),
            "other/dir_target/c.txt = CCC".to_owned(),
            "sibling.txt = SSS".to_owned(),
        ],
        "`<dir>/..` is DOTDIR_NAME (flist.c:2595-2602), so the parent's contents \
         land directly in the destination - upstream rsync 3.5.0 exits 0 with \
         exactly this tree",
    );
}

/// The `..` spelling is indistinguishable from the two DOTDIR spellings oc
/// already handled. Pinning the EQUALITY rather than a second literal keeps the
/// three from drifting apart.
#[test]
fn parent_dir_operand_matches_the_trailing_slash_and_dot_spellings() {
    let mut seen: Vec<(&str, Vec<String>)> = Vec::new();
    for suffix in ["/..", "/../", "/../.", "/./.."] {
        let (_temp, base, to) = fixture();
        let operand = PathBuf::from(format!("{}/from{suffix}", base.display()));
        seen.push((suffix, run(&operand, &to, options())));
    }

    let (first_suffix, first) = &seen[0];
    for (suffix, rows) in &seen[1..] {
        assert_eq!(
            rows, first,
            "`from{suffix}` and `from{first_suffix}` are both DOTDIR operands \
             (flist.c:2584-2604) and must deliver the same tree",
        );
    }
}

/// `..` after a symlink resolves through it, so `from/sym-to-dir/..` is
/// `other`, NOT `from`. A fix that special-cased the operand's own lexical
/// parent would deliver `from`'s contents here.
#[test]
fn parent_dir_operand_resolves_through_a_symlinked_component() {
    let (_temp, base, to) = fixture();

    let got = run(&base.join("from/sym-to-dir/.."), &to, options());

    assert_eq!(
        got,
        vec![
            "dir_target/".to_owned(),
            "dir_target/c.txt = CCC".to_owned(),
        ],
        "`from/sym-to-dir/..` names `other` - the kernel resolves `..` from the \
         symlink's TARGET, and upstream chdir()s to the operand verbatim \
         (flist.c:2610-2621, flist.c:2677-2679)",
    );
}

/// NEGATIVE CONTROL. An ordinary directory operand is `NORMAL_NAME`
/// (`flist.c:2605-2606`): it lands under its own name. Nothing in this cell
/// touches the trailing-`..` rule, so it must stay green whether that rule is
/// present or reverted.
#[test]
fn an_ordinary_directory_operand_still_lands_under_its_own_name() {
    let (_temp, base, to) = fixture();

    let got = run(&base.join("from"), &to, options());

    assert_eq!(
        got,
        vec![
            "from/".to_owned(),
            "from/a.txt = AAA".to_owned(),
            "from/sub/".to_owned(),
            "from/sub/b.txt = BBB".to_owned(),
            "from/sym-to-dir -> ../other/dir_target".to_owned(),
        ],
        "a NORMAL_NAME operand keeps its basename",
    );
}

/// A `..` that is not the LAST component is `NORMAL_NAME`. This is the cell a
/// fix that simply always appended `/.`, or that tested `contains("..")`, would
/// fail: `other` would arrive as loose contents instead of a directory.
#[test]
fn a_dotdot_that_is_not_the_last_component_is_a_normal_name() {
    let (_temp, base, to) = fixture();

    let got = run(&base.join("from/../other"), &to, options());

    assert_eq!(
        got,
        vec![
            "other/".to_owned(),
            "other/dir_target/".to_owned(),
            "other/dir_target/c.txt = CCC".to_owned(),
        ],
        "upstream's guard is `fbuf[len-1] == '.' && fbuf[len-2] == '.' && \
         (len == 2 || fbuf[len-3] == '/')` (flist.c:2595-2596) - an interior \
         `..` never reaches the append",
    );
}

/// A file operand whose name merely ENDS in the two bytes `..` is not a
/// `ParentDir` component. `fbuf[len-3] == '/'` is what upstream requires.
#[test]
fn a_basename_ending_in_two_dots_is_not_a_parent_dir_component() {
    let (_temp, base, to) = fixture();
    fs::write(base.join("from/weird.."), b"WW").expect("write weird..");

    let got = run(&base.join("from/weird.."), &to, options());

    assert_eq!(
        got,
        vec!["weird.. = WW".to_owned()],
        "`weird..` has `fbuf[len-3] == 'd'`, so flist.c:2595-2596 does not fire",
    );
}

/// The rule is scoped to NON-relative operands, because upstream's marker chain
/// is: `flist.c:2581-2583` forces `NORMAL_NAME` under `--relative` before the
/// `..` arm is reached, and `flist.c:2658-2667` rejects the operand outright
/// with `found ".." dir in relative path: %s` / `exit_cleanup(RERR_SYNTAX)`.
///
/// oc does not implement that rejection yet (MEASURED: rsync 3.5.0 exits 1,
/// oc exits 23 - a KNOWN residual divergence, unchanged by this fix). What this
/// cell pins is only the SCOPE: `--relative` must not quietly acquire contents
/// semantics, which upstream never grants it.
#[test]
fn parent_dir_rule_is_scoped_to_non_relative() {
    let (_temp, base, to) = fixture();
    let operands = vec![
        OsString::from(base.join("from/..").as_os_str()),
        OsString::from(to.as_os_str()),
    ];
    let relative = options().relative_paths(true);
    let plan = LocalCopyPlan::from_operands_with_relative(&operands, true).expect("plan");

    let result = plan.execute_with_report(LocalCopyExecution::Apply, relative);

    assert!(
        result.is_err(),
        "under `--relative` upstream REJECTS a `..` operand (flist.c:2658-2667); \
         it must not be granted the DOTDIR contents semantics of the \
         non-relative arm. Destination held: {:?}",
        snapshot(&to),
    );
}
