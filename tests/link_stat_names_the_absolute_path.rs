//! Regression: a local copy's `link_stat` diagnostic names the source operand
//! by its absolute path, the way upstream's `full_fname()` does.
//!
//! Upstream renders every path inside a sender diagnostic through
//! `full_fname()` (`util1.c:1433-1464`), which prefixes a relative `fn` with
//! `curr_dir`:
//!
//! ```c
//! if (*fn == '/')
//!         p1 = p2 = "";
//! else {
//!         p1 = curr_dir + module_dirlen;
//!         for (p2 = p1; *p2 == '/'; p2++) {}
//!         if (*p2)
//!                 p2 = "/";
//! }
//! ```
//!
//! `send_file_list()` has already split the operand into a `dir`/`fn` pair and
//! `push_dir()`ed into `dir` before the `link_stat()` at `flist.c:2697`, so
//! `curr_dir` is the operand's parent and the rendered name is absolute. From
//! `/tmp/t1158`, rsync 3.5.0 reports `-a nope dst/` as:
//!
//! ```text
//! rsync: [sender] link_stat "/tmp/t1158/nope" failed: No such file or directory (2)
//! ```
//!
//! oc reported `link_stat "nope" failed` - the operand as typed - which is what
//! this file pins. The name is built once, at the point the engine constructs
//! `LocalCopyError::link_stat_failed`, and reaches stderr through two emitters
//! (the engine's own `eprintln!` of the error and the `ClientError` the same
//! error is mapped to), so both lines are asserted here rather than one.
//!
//! These are end-to-end runs of the shipped binary: the operand name is
//! produced on the live local-copy path, and a unit test over the helper alone
//! could not show that the path reaches the diagnostic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// Upstream `RERR_PARTIAL`: some files were not transferred (`errcode.h`).
const RERR_PARTIAL: i32 = 23;

/// Path to the binary under test, resolved by Cargo at compile time.
fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// A scratch tree plus the *canonical* spelling of its root.
///
/// `getcwd()` - which both upstream and oc read for `curr_dir` - returns the
/// resolved path, and a temporary directory commonly sits under a symlinked
/// ancestor (`/var` -> `/private/var` on macOS). Comparing against the
/// unresolved `TempDir` path would fail there for a reason that has nothing to
/// do with the diagnostic.
struct Scratch {
    _root: TempDir,
    working_dir: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let working_dir = fs::canonicalize(root.path()).expect("canonicalize tempdir");
        fs::create_dir_all(working_dir.join("dst")).expect("create dst");
        fs::create_dir_all(working_dir.join("sub")).expect("create sub");
        Self {
            _root: root,
            working_dir,
        }
    }
}

/// Runs one local copy of `operand` from `working_dir` and returns the result.
fn run_from(working_dir: &Path, operand: &str) -> Output {
    Command::new(oc_rsync_binary())
        .arg("-a")
        .arg(operand)
        .arg("dst/")
        .current_dir(working_dir)
        .output()
        .expect("run oc-rsync")
}

/// Asserts the run reported the missing operand under `expected_name` and
/// exited `RERR_PARTIAL`.
///
/// Every `link_stat` line is checked, not just the first: the error is rendered
/// twice on this path, and asserting only that the text appears *somewhere*
/// would stay green if one emitter kept the old name.
fn assert_named(output: &Output, expected_name: &Path, label: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!("link_stat \"{}\" failed", expected_name.display());

    let reports: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("link_stat"))
        .collect();
    assert!(
        !reports.is_empty(),
        "{label}: no link_stat diagnostic at all; stderr was:\n{stderr}"
    );
    for line in &reports {
        assert!(
            line.contains(&expected),
            "{label}: expected a line containing `{expected}`, got `{line}`"
        );
    }

    assert_eq!(
        output.status.code(),
        Some(RERR_PARTIAL),
        "{label}: a missing source operand exits {RERR_PARTIAL}; stderr was:\n{stderr}"
    );
}

#[test]
fn a_relative_operand_is_reported_by_its_absolute_path() {
    let scratch = Scratch::new();
    let output = run_from(&scratch.working_dir, "nope");
    assert_named(&output, &scratch.working_dir.join("nope"), "bare relative");
}

#[test]
fn a_relative_operand_with_a_parent_keeps_the_whole_tail() {
    let scratch = Scratch::new();
    let output = run_from(&scratch.working_dir, "sub/nope");
    assert_named(
        &output,
        &scratch.working_dir.join("sub").join("nope"),
        "relative with parent",
    );
}

#[test]
fn dot_and_dot_dot_components_collapse_lexically() {
    let scratch = Scratch::new();

    let output = run_from(&scratch.working_dir, "./nope");
    assert_named(&output, &scratch.working_dir.join("nope"), "leading ./");

    // `push_dir("..")` from `sub/` lands back at the scratch root, exactly as
    // `clean_fname()` cancels the component before a `..`.
    let output = run_from(&scratch.working_dir.join("sub"), "../nope");
    assert_named(&output, &scratch.working_dir.join("nope"), "leading ../");
}

/// Negative control. An absolute operand already matched upstream before the
/// working directory was consulted, so this leg must stay green under any
/// mutation of the relative arm - it is what distinguishes "the name is now
/// anchored" from "the name changed".
#[test]
fn an_absolute_operand_is_reported_unchanged() {
    let scratch = Scratch::new();
    let absolute = scratch.working_dir.join("nope");
    let output = run_from(&scratch.working_dir, &absolute.display().to_string());
    assert_named(&output, &absolute, "absolute operand");
}

/// Locates a real upstream rsync 3.x, either where the interop harness installs
/// it or where it builds it, and verifies the `--version` banner so macOS's
/// `openrsync` shim cannot stand in for it.
fn upstream_rsync() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    for version in ["3.5.0", "3.4.4"] {
        if let Some(installed) = test_support::upstream_install_bin(version) {
            candidates.push(installed);
        }
        if let Some(root) = test_support::workspace_root() {
            candidates.push(
                root.join("target")
                    .join("interop")
                    .join("upstream-src")
                    .join(format!("rsync-{version}"))
                    .join("rsync"),
            );
        }
    }

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .is_some_and(|text| {
                text.lines()
                    .next()
                    .is_some_and(|line| line.starts_with("rsync") && line.contains(" version 3."))
            })
    })
}

/// Extracts the quoted name from the first `link_stat` line, if any.
fn quoted_link_stat_name(output: &Output) -> Option<String> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().find(|line| line.contains("link_stat "))?;
    let rest = line.split_once("link_stat \"")?.1;
    let (name, _) = rest.split_once('"')?;
    Some(name.to_owned())
}

/// Cross-implementation leg: the quoted name must be the one a real rsync
/// prints for the same operand from the same directory. Skipped when no
/// upstream binary is installed; the literal assertions above are what hold the
/// behaviour in that case.
#[test]
fn the_quoted_name_matches_a_real_rsync() {
    let Some(upstream) = upstream_rsync() else {
        eprintln!("skip: no upstream rsync 3.x binary available");
        return;
    };

    for operand in ["nope", "sub/nope", "./nope"] {
        let scratch = Scratch::new();

        let ours = run_from(&scratch.working_dir, operand);
        let theirs = Command::new(&upstream)
            .arg("-a")
            .arg(operand)
            .arg("dst/")
            .current_dir(&scratch.working_dir)
            .output()
            .expect("run upstream rsync");

        let expected = quoted_link_stat_name(&theirs)
            .unwrap_or_else(|| panic!("upstream printed no link_stat line for `{operand}`"));
        let actual = quoted_link_stat_name(&ours)
            .unwrap_or_else(|| panic!("oc-rsync printed no link_stat line for `{operand}`"));

        assert_eq!(
            actual, expected,
            "operand `{operand}`: oc must name the operand exactly as upstream does"
        );
    }
}
