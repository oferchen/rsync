//! The `:e` dir-merge modifier hides the merge file's own name.
//!
//! upstream: `exclude.c:1558-1571` synthesises the exclude at REGISTRATION -
//! when the directive is parsed - into the ENCLOSING list, then clears
//! `FILTRULE_EXCLUDE_SELF` so it is added exactly once. oc used to build it at
//! LOAD time instead, once per directory that turned out to contain a file by
//! that name, which made the rule conditional on the file existing.
//!
//! ## Why the fixture uses a bracket glob
//!
//! `:e name` excluding a file literally called `name` is not a discriminating
//! shape: if no such file exists there is nothing to observe either way. The
//! fixture below mirrors upstream's own `filter-merge-content-echo` cell and
//! names the merge file `excl-self-[x]9`, a glob whose LITERAL text matches the
//! real file `excl-self-x9`. Nothing is ever named `excl-self-[x]9`, so the
//! merge file is never found - and the synthesised exclude still has to fire.
//!
//! ## What a duplicate would (not) look like
//!
//! Registering the exclude twice is not observable: filter lists are
//! first-match-wins and both copies are the same exclude. So these cells pin
//! the direction that IS observable - the exclude firing where oc used to skip
//! it - and the file-present cell guards against the opposite regression, the
//! load-time removal silencing a path that already worked.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

/// The merge-file name as written in the directive: a glob, deliberately.
const SELF_GLOB: &str = "excl-self-[x]9";
/// The real file the glob matches. No file is ever named `SELF_GLOB` itself.
const MATCHED_FILE: &str = "excl-self-x9";

/// Builds `<dir>/src` holding `f` plus the glob-matched file.
fn seed_source(dir: &TestDir) -> std::io::Result<()> {
    dir.mkdir("src")?;
    dir.mkdir("dest")?;
    dir.write_file("src/f", b"keep\n")?;
    dir.write_file(&format!("src/{MATCHED_FILE}"), b"hidden\n")?;
    Ok(())
}

/// A `--filter=':e NAME'` given on the command line must hide `NAME` even
/// though no directory in the transfer holds a file by that name.
///
/// This is the arm oc got wrong: the exclude was created only when the named
/// merge file was actually found and loaded.
#[test]
fn a_top_level_exclude_self_fires_without_the_merge_file() {
    let dir = TestDir::new().expect("scratch dir");
    seed_source(&dir).expect("fixture");

    RsyncCommand::new()
        .arg("-r")
        .arg(format!("--filter=:e {SELF_GLOB}"))
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()))
        .assert_success();

    assert!(
        dir.exists("dest/f"),
        "the unrelated file must still transfer"
    );
    assert!(
        !dir.exists(&format!("dest/{MATCHED_FILE}")),
        "`:e {SELF_GLOB}` must hide {MATCHED_FILE}: upstream adds the exclude \
         when the directive is PARSED, not when a file by that name is found"
    );
}

/// Non-vacuity companion for the cell above: with the merge file actually
/// present, the exclude must still fire and the file must still transfer.
///
/// Without this, removing the load-time push could silence a path that used to
/// work and the first cell alone would not notice.
#[test]
fn a_top_level_exclude_self_still_hides_a_merge_file_that_exists() {
    let dir = TestDir::new().expect("scratch dir");
    dir.mkdir("src").expect("src");
    dir.mkdir("dest").expect("dest");
    dir.write_file("src/f", b"keep\n").expect("f");
    dir.write_file("src/mergefile", b"# empty\n")
        .expect("mergefile");

    RsyncCommand::new()
        .arg("-r")
        .arg("--filter=:e mergefile")
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()))
        .assert_success();

    assert!(
        dir.exists("dest/f"),
        "the unrelated file must still transfer"
    );
    assert!(
        !dir.exists("dest/mergefile"),
        "a merge file that IS present must still be hidden by its own `:e`"
    );
}

/// The same rule read out of a per-directory merge file: the exclude belongs to
/// the enclosing `.rsync-filter`'s scope, and again does not wait for a file
/// named `SELF_GLOB` to appear.
///
/// This is the shape upstream's `filter-merge-content-echo` cell exercises.
#[test]
fn a_nested_exclude_self_fires_without_the_merge_file() {
    let dir = TestDir::new().expect("scratch dir");
    seed_source(&dir).expect("fixture");
    dir.write_file("src/.rsync-filter", format!(":e {SELF_GLOB}\n").as_bytes())
        .expect("per-dir filter");

    RsyncCommand::new()
        .arg("-r")
        .arg("-F")
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()))
        .assert_success();

    assert!(
        dir.exists("dest/f"),
        "the unrelated file must still transfer"
    );
    assert!(
        !dir.exists(&format!("dest/{MATCHED_FILE}")),
        "a `:e {SELF_GLOB}` nested in .rsync-filter must hide {MATCHED_FILE}"
    );
}

/// The name upstream excludes is the BASENAME, not the path it was written
/// with: `:e sub/NAME` hides `NAME` in each directory, never `sub/NAME`.
///
/// Pins the scan at `exclude.c:1568-1569` end to end rather than only in the
/// `filters` unit test that owns it.
#[test]
fn a_qualified_exclude_self_hides_the_basename() {
    let dir = TestDir::new().expect("scratch dir");
    seed_source(&dir).expect("fixture");

    RsyncCommand::new()
        .arg("-r")
        .arg(format!("--filter=:e sub/{SELF_GLOB}"))
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()))
        .assert_success();

    assert!(
        !dir.exists(&format!("dest/{MATCHED_FILE}")),
        "the exclude carries only the last path segment, so it matches \
         {MATCHED_FILE} at the transfer root"
    );
}
