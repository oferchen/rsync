//! A merge directive that names a side must refuse a rule inside the file that
//! also names one.
//!
//! upstream: exclude.c:1447-1456
//!
//! ```c
//! if (template->rflags & FILTRULES_SIDES) {
//!     if (rule->rflags & FILTRULES_SIDES) {
//!         /* The filter and template both specify side(s).  This
//!          * is dodgy (and won't work correctly if the template is
//!          * a one-sided per-dir merge rule), so reject it. */
//!         filter_rule_err("specified-side merge file contains specified-side filter",
//!                         *rulestr_ptr);
//!     }
//!     rule->rflags |= template->rflags & FILTRULES_SIDES;
//! }
//! ```
//!
//! `filter_rule_err` is fatal - it calls `exit_cleanup(RERR_SYNTAX)`
//! (exclude.c:133-137), i.e. exit 1. The refusal sits ABOVE the inherit, so a
//! conflicting side is never applied.
//!
//! Every expectation below was MEASURED against a real rsync 3.5.0 binary
//! before the fix was written; oc exited 0 and silently applied the rule in
//! the three conflict rows.
//!
//! ⚠ oc reaches this condition through TWO independent code paths, because it
//! re-implements the template-to-rule side inheritance where upstream has a
//! single `parse_rule_tok` site: `.s FILE` (merge) goes through
//! `cli/frontend/filter_rules/merge.rs`, while `:s FILE` (dir-merge) goes
//! through `engine/local_copy/dir_merge/load.rs`. Fixing only one leaves the
//! other silently accepting, which is why both shapes are pinned here rather
//! than just the one the bug was filed against.

#![cfg(unix)]

use std::fs;
use std::process::Command;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    /// Builds `src/{foo,bar}` plus a rules file holding `rule_line`, written
    /// both at the top level (for `.` merge) and inside `src/` (for `:`
    /// dir-merge, which is read per-directory during traversal).
    fn new(rules_name: &str, rule_line: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::create_dir_all(root.join("dst")).expect("mkdir dst");
        fs::write(root.join("src/foo"), b"hello\n").expect("write foo");
        fs::write(root.join("src/bar"), b"world\n").expect("write bar");
        let body = format!("{rule_line}\n");
        fs::write(root.join(rules_name), &body).expect("write rules");
        fs::write(root.join("src").join(rules_name), &body).expect("write src rules");
        Self { dir }
    }

    fn run(&self, filter: &str) -> (i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
            .current_dir(self.dir.path())
            .arg("-r")
            .arg(format!("--filter={filter}"))
            .arg("src/")
            .arg("dst/")
            .output()
            .expect("spawn oc-rsync");
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.code().unwrap_or(-1), text)
    }
}

const MESSAGE: &str = "specified-side merge file contains specified-side filter";

fn assert_refused(filter: &str, rules_name: &str, rule_line: &str) {
    let fixture = Fixture::new(rules_name, rule_line);
    let (code, text) = fixture.run(filter);
    assert_eq!(
        code, 1,
        "--filter={filter} with '{rule_line}' must exit 1 (upstream RERR_SYNTAX); got {code}, output: {text}"
    );
    assert!(
        text.contains(MESSAGE),
        "--filter={filter} with '{rule_line}' must report upstream's wording; got: {text}"
    );
}

fn assert_accepted(filter: &str, rules_name: &str, rule_line: &str) {
    let fixture = Fixture::new(rules_name, rule_line);
    let (code, text) = fixture.run(filter);
    assert_eq!(
        code, 0,
        "--filter={filter} with '{rule_line}' must succeed; got {code}, output: {text}"
    );
    assert!(
        !text.contains(MESSAGE),
        "--filter={filter} with '{rule_line}' must not trip the side-conflict guard; got: {text}"
    );
}

#[test]
fn sided_merge_refuses_a_receiver_sided_rule() {
    assert_refused(".s f.rules", "f.rules", "-r foo");
}

#[test]
fn receiver_sided_merge_refuses_a_sender_sided_rule() {
    // The mirror direction: upstream's test is on the SIDES mask, not on one
    // particular side, so `.r` + `-s` must refuse exactly like `.s` + `-r`.
    assert_refused(".r f.rules", "f.rules", "-s foo");
}

#[test]
fn sided_dir_merge_refuses_a_sided_rule() {
    // Distinct code path from the `.` merge above: the per-directory file is
    // read during traversal by the local-copy dir-merge loader.
    assert_refused(":s .rules", ".rules", "-r foo");
}

#[test]
fn sided_merge_refuses_a_protect_rule() {
    // `P` sets FILTRULE_RECEIVER_SIDE upstream, so it counts as a specified
    // side even though no `s`/`r` modifier is typed. Measured: rsync 3.5.0
    // exits 1 here.
    assert_refused(".s f.rules", "f.rules", "P foo");
}

#[test]
fn sided_merge_refuses_a_risk_rule() {
    assert_refused(".s f.rules", "f.rules", "R foo");
}

// --- non-vacuity controls -------------------------------------------------
//
// Without these a build that refused EVERY merge rule, or refused on any
// merge at all, would still satisfy every row above.

#[test]
fn sided_merge_accepts_an_unsided_rule() {
    assert_accepted(".s f.rules", "f.rules", "- foo");
}

#[test]
fn sided_dir_merge_accepts_an_unsided_rule() {
    assert_accepted(":s .rules", ".rules", "- foo");
}

#[test]
fn unsided_merge_accepts_a_sided_rule() {
    // The template carries no side, so upstream's outer `if` never fires and
    // the rule's own side is simply honoured.
    assert_accepted(". f.rules", "f.rules", "-r foo");
}

#[test]
fn perishable_modifier_is_not_a_side() {
    // `p` is the perishable modifier, not a side. Measured: rsync 3.5.0 exits
    // 0. This pins the guard against keying on "the directive has modifiers"
    // instead of "the directive names a side".
    assert_accepted(":p .rules", ".rules", "-r foo");
}
