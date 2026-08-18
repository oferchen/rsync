//! The `x` filter modifier is legal on a dir-merge prefix and does not make the
//! merged rules xattr rules.
//!
//! Upstream's modifier loop is `- + / ! C e n p r s w x`. Most arms open with a
//! `goto invalid` guard restricting them to certain rule kinds; exactly three -
//! `/`, `p` and `x` - have none:
//!
//! ```c
//! case 'x':
//!         rule->rflags |= FILTRULE_XATTR;
//!         saw_xattr_filter = 1;
//!         break;
//! ```
//!
//! (`exclude.c:1438`). Nothing restricts it to a rule prefix, so `:x FILE` and
//! `.x FILE` parse exactly like `: FILE` and `. FILE` plus the XATTR bit on the
//! *container* rule.
//!
//! The bit is deliberately not inherited by the rules read out of the merged
//! file. Upstream computes what a container passes down as
//!
//! ```c
//! #define FILTRULES_FROM_CONTAINER (FILTRULE_ABS_PATH | FILTRULE_INCLUDE \
//!                                 | FILTRULE_DIRECTORY | FILTRULE_NEGATE \
//!                                 | FILTRULE_PERISHABLE)
//! ```
//!
//! (`exclude.c:1229`) - `FILTRULE_XATTR` is absent. Propagating it would turn
//! every rule in the merged file into an xattr rule, which matches attribute
//! names rather than paths, so a `- FOO` inside the file would stop excluding
//! the file `FOO`.
//!
//! Both expectations below were captured from rsync 3.5.0 run over the same
//! trees: `.rules KEEP` for the flat fixture (with and without the `x`) and
//! `.rules .rules2 KEEP` for the nested one, each at exit 0.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Source tree whose `.rules` is itself the rule file: `- FOO`.
///
/// Reached through a `--filter` directive, so this fixture exercises the
/// command-line filter parser.
fn flat_tree() -> TempDir {
    let tmp = new_tree();
    fs::write(tmp.path().join("src/.rules"), b"- FOO\n").expect("rule file");
    tmp
}

/// Source tree whose `.rules` carries its own `:x` dir-merge directive.
///
/// The `x` therefore appears in the *contents* of a merged file rather than on
/// the command line, which is a separate parser from the one `flat_tree`
/// reaches.
fn nested_tree() -> TempDir {
    let tmp = new_tree();
    fs::write(tmp.path().join("src/.rules"), b":x .rules2\n").expect("outer rule file");
    fs::write(tmp.path().join("src/.rules2"), b"- FOO\n").expect("inner rule file");
    tmp
}

/// `src/` holding the file the rules exclude plus one they do not, and `dst/`.
///
/// `KEEP` is what makes the assertions non-vacuous: without it an empty
/// destination would satisfy every expectation below, including the one where
/// the client fails to start at all.
fn new_tree() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("source root");
    fs::create_dir_all(tmp.path().join("dst")).expect("destination root");
    fs::write(tmp.path().join("src/FOO"), b"foo\n").expect("excluded file");
    fs::write(tmp.path().join("src/KEEP"), b"keep\n").expect("retained file");
    tmp
}

/// Runs `oc-rsync -r --filter=<rule> src/ dst/` and returns the exit code with
/// the sorted destination listing.
///
/// The exit code is returned alongside the listing because the listing alone
/// cannot tell a working exclusion from a client that refused to parse the
/// rule - both leave `FOO` out of the destination.
fn transfer(root: &Path, filter: &str) -> (i32, Vec<String>) {
    let mut source = root.join("src").into_os_string();
    source.push("/");
    let mut destination = root.join("dst").into_os_string();
    destination.push("/");

    let status = Command::new(oc_rsync_bin())
        .arg("-r")
        .arg(format!("--filter={filter}"))
        .arg(source)
        .arg(destination)
        .status()
        .expect("run oc-rsync");

    let mut entries: Vec<String> = fs::read_dir(root.join("dst"))
        .expect("read destination")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();

    (status.code().expect("exit code"), entries)
}

#[test]
fn directive_x_modifier_is_accepted_and_merges_the_named_file() {
    let tmp = flat_tree();
    assert_eq!(
        transfer(tmp.path(), ":x .rules"),
        (0, vec![".rules".to_string(), "KEEP".to_string()]),
    );
}

#[test]
fn directive_x_modifier_does_not_change_what_the_merged_rules_match() {
    let with_x = flat_tree();
    let without_x = flat_tree();
    assert_eq!(
        transfer(with_x.path(), ":x .rules"),
        transfer(without_x.path(), ": .rules"),
        "FILTRULES_FROM_CONTAINER omits XATTR, so `x` must not reach the merged rules",
    );
}

#[test]
fn nested_x_modifier_is_accepted_inside_a_merged_file() {
    let tmp = nested_tree();
    assert_eq!(
        transfer(tmp.path(), ": .rules"),
        (
            0,
            vec![
                ".rules".to_string(),
                ".rules2".to_string(),
                "KEEP".to_string(),
            ],
        ),
    );
}
