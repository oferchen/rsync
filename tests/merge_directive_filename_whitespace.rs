//! A merge directive's FILENAME keeps its trailing whitespace.
//!
//! Sibling of `filter_rule_whitespace_is_pattern_text.rs`, which covers the
//! rule PATTERN. This file covers the other half: the file NAME a `merge` /
//! `dir-merge` directive points at.
//!
//! `parse_rule_tok` consumes exactly ONE separator after the rule character or
//! keyword (`exclude.c:1444-1445`, `if (*s) s++`) and then takes the rest of
//! the rule verbatim, `len = strlen((char*)s)` (`exclude.c:1465`). The merge
//! name reaches `parse_merge_name` (`exclude.c:696-752`), whose only transform
//! is `clean_fname` (`:734`) - it collapses `//` and `..`, and never touches
//! whitespace. So `: .rsync-filter ` names a merge file whose last byte is a
//! space, and a trailing space is a legal byte in a filename.
//!
//! MEASURED against rsync 3.5.0 over a source holding `a`, `b` and a filter
//! file literally named `.rsync-filter ` (trailing space) reading `- b`:
//!
//! | directive                         | upstream 3.5.0 | oc before        |
//! |-----------------------------------|----------------|------------------|
//! | `--filter=': .rsync-filter '`     | copies `a`     | copies `a` + `b` |
//! | `--filter='dir-merge .rsync-... '`| copies `a`     | copies `a` + `b` |
//! | `--filter='merge DIR/.rsync-... '`| copies `a`     | exit 11          |
//! | `.rsync-filter` holding `: inner `| copies `a`     | copies `a` + `b` |
//! | `.rsync-filter` holding `merge inner ` | copies `a` | exit 24          |
//!
//! Five readers had to agree and did not: the CLI short `.`/`:` parser, the
//! CLI long `merge` and `dir-merge` parsers, and the engine's short and long
//! merge parsers inside a `.rsync-filter` file. The engine's long `dir-merge`
//! parser already matched upstream, which is what made this a per-reader drift
//! rather than a uniform choice.
//!
//! Every cell asserts on the DESTINATION TREE, so none of them can pass by
//! parsing the name "correctly" into a string nobody opens.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

/// The file the merge rule excludes when the merge file is found.
const EXCLUDED: &str = "b";
/// The file no cell may exclude: the negative control inside every fixture.
const KEPT: &str = "a";

/// A filter file whose name ends in a space.
const FILTER_SPACED: &str = ".rsync-filter ";
/// A nested filter file whose name ends in a space.
const INNER_SPACED: &str = "inner ";

/// Seeds `<dir>/src` with `a` and `b` plus an empty `<dir>/dest`.
fn seed(dir: &TestDir) {
    dir.mkdir("src").expect("src");
    dir.mkdir("dest").expect("dest");
    dir.write_file("src/a", b"keep\n").expect("a");
    dir.write_file("src/b", b"drop\n").expect("b");
}

/// Runs `oc-rsync -r --filter=<rule> src/ dest/` inside `dir`.
fn transfer(dir: &TestDir, rule: &str) -> RsyncCommand {
    let mut command = RsyncCommand::new();
    command
        .arg("-r")
        .arg(format!("--filter={rule}"))
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()));
    command
}

/// Asserts the merge file was found and honoured: `b` excluded, `a` copied.
fn assert_merge_applied(dir: &TestDir, rule: &str) {
    assert!(
        !dir.exists(&format!("dest/{EXCLUDED}")),
        "`{rule}` must find the merge file whose name ends in a space and \
         honour its `- {EXCLUDED}` rule (exclude.c:1465, :734)"
    );
    assert!(
        dir.exists(&format!("dest/{KEPT}")),
        "`{rule}` must not exclude `{KEPT}`"
    );
}

/// `: NAME ` names a merge file whose last byte is a space.
///
/// The headline cell for the CLI short form. Trimming made oc look for
/// `.rsync-filter`, find nothing, and copy `b`.
#[test]
fn a_short_dir_merge_keeps_its_filenames_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file(&format!("src/{FILTER_SPACED}"), b"- b\n")
        .expect("filter");

    let rule = ": .rsync-filter ";
    transfer(&dir, rule).assert_success();
    assert_merge_applied(&dir, rule);
}

/// `dir-merge NAME ` behaves the same as its `:` short form.
#[test]
fn a_long_dir_merge_keeps_its_filenames_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file(&format!("src/{FILTER_SPACED}"), b"- b\n")
        .expect("filter");

    let rule = "dir-merge .rsync-filter ";
    transfer(&dir, rule).assert_success();
    assert_merge_applied(&dir, rule);
}

/// `merge PATH ` opens the file at `PATH ` - trailing space included.
///
/// Trimming made oc open `PATH` instead, which does not exist, and abort with
/// `failed to open exclude file` (exit 11) where upstream exits 0.
#[test]
fn a_long_merge_keeps_its_paths_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file(&format!("src/{FILTER_SPACED}"), b"- b\n")
        .expect("filter");

    let path = dir.path().join("src").join(FILTER_SPACED);
    let rule = format!("merge {}", path.display());
    transfer(&dir, &rule).assert_success();
    assert_merge_applied(&dir, &rule);
}

/// `. PATH ` (the short merge form) keeps the trailing space too.
#[test]
fn a_short_merge_keeps_its_paths_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file(&format!("src/{FILTER_SPACED}"), b"- b\n")
        .expect("filter");

    let path = dir.path().join("src").join(FILTER_SPACED);
    let rule = format!(". {}", path.display());
    transfer(&dir, &rule).assert_success();
    assert_merge_applied(&dir, &rule);
}

/// The ENGINE parser: a `: inner ` line INSIDE a `.rsync-filter` file.
///
/// This cell reaches `crates/engine/.../dir_merge/parse/merge.rs`, a different
/// parser from the four above. It trimmed independently.
#[test]
fn a_nested_short_dir_merge_keeps_its_filenames_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b": inner \n")
        .expect("outer");
    dir.write_file(&format!("src/{INNER_SPACED}"), b"- b\n")
        .expect("inner");

    let rule = ": .rsync-filter";
    transfer(&dir, rule).assert_success();
    assert_merge_applied(&dir, rule);
}

/// The ENGINE parser's long form: a `merge inner ` line inside a
/// `.rsync-filter` file. Trimming aborted the transfer with exit 24.
#[test]
fn a_nested_long_merge_keeps_its_paths_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b"merge inner \n")
        .expect("outer");
    dir.write_file(&format!("src/{INNER_SPACED}"), b"- b\n")
        .expect("inner");

    let rule = ": .rsync-filter";
    transfer(&dir, rule).assert_success();
    assert_merge_applied(&dir, rule);
}

/// The ENGINE parser's short merge form: a `. inner ` line inside a
/// `.rsync-filter` file.
#[test]
fn a_nested_short_merge_keeps_its_paths_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b". inner \n")
        .expect("outer");
    dir.write_file(&format!("src/{INNER_SPACED}"), b"- b\n")
        .expect("inner");

    let rule = ": .rsync-filter";
    transfer(&dir, rule).assert_success();
    assert_merge_applied(&dir, rule);
}

/// A name with a trailing space must NOT find the file without one.
///
/// The discriminating direction: the cells above would also pass if the parser
/// tried both spellings. Here only the unspaced file exists, so a name that
/// keeps its space must find nothing and `b` must transfer.
#[test]
fn a_spaced_name_does_not_match_the_unspaced_file() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b"- b\n")
        .expect("filter");

    transfer(&dir, ": .rsync-filter ").assert_success();

    assert!(
        dir.exists(&format!("dest/{EXCLUDED}")),
        "`: .rsync-filter ` names `.rsync-filter ` (with the space), which \
         does not exist here, so no rule applies and `{EXCLUDED}` transfers"
    );
    assert!(
        dir.exists(&format!("dest/{KEPT}")),
        "`{KEPT}` must transfer as well"
    );
}

/// The unchanged baseline: a merge directive with no stray whitespace still
/// works.
///
/// This is the control that must stay GREEN under the mutation that restores
/// the trimming - a cell that flips both ways would prove nothing about the
/// cells above.
#[test]
fn a_plain_merge_directive_is_unaffected() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b"- b\n")
        .expect("filter");

    let rule = ": .rsync-filter";
    transfer(&dir, rule).assert_success();
    assert_merge_applied(&dir, rule);
}
