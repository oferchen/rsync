//! Interop validation for the four side-specific filter modifiers
//! `protect`/`P`, `risk`/`R`, `hide`/`H`, and `show`/`S`, exercised through a
//! real engine local-copy transfer (sender file-list build + receiver delete
//! pass) and asserted against the outcome captured from upstream rsync 3.4.4.
//!
//! Unlike the library-level checks in `crates/filters/tests/`, these cases run
//! the full copy+delete pipeline so the assertion is on the resulting
//! destination tree - the same observable that `diff -r` compares between
//! oc-rsync and upstream. Each test documents the exact upstream CLI it was
//! captured from and cites the upstream C that defines the semantic.
//!
//! # Upstream semantics (rsync 3.4.4 `exclude.c`)
//!
//! The four modifiers are parsed at `exclude.c:1194-1207`:
//!
//! - `S`/`show` -> `FILTRULE_INCLUDE | FILTRULE_SENDER_SIDE` (a sender-side
//!   include: it re-shows a file a broader `hide` would omit).
//! - `H`/`hide` -> `FILTRULE_SENDER_SIDE` (a sender-side exclude: the sender
//!   omits the file from the transferred file list).
//! - `R`/`risk` -> `FILTRULE_INCLUDE | FILTRULE_RECEIVER_SIDE` (a receiver-side
//!   include: it re-exposes to `--delete` a file a broader `protect` guards).
//! - `P`/`protect` -> `FILTRULE_RECEIVER_SIDE` (a receiver-side exclude: the
//!   receiver keeps the matching destination file during the `--delete` pass).
//!
//! The side of a rule selects WHEN it is consulted: `send_rules()`
//! (`exclude.c:1605-1613`) elides receiver-side rules from the sender's
//! evaluation and sender-side rules from the receiver's, and `rule_matches()`
//! skips rules whose `elide` matches the current side (`exclude.c:911`). Net
//! effect: `H`/`S` shape the sender file list; `P`/`R` shape the receiver
//! delete pass. That is precisely the split oc-rsync implements - `hide`/`show`
//! carry `applies_to_sender` and are checked in the `Transfer` context, while
//! `protect`/`risk` carry `applies_to_receiver` and are checked in the
//! `Deletion` context (`crates/filters/src/rule.rs`,
//! `crates/filters/src/decision.rs`).

use std::fs;
use std::path::Path;

use engine::local_copy::{
    FilterProgram, FilterProgramEntry, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
};
use filters::FilterRule;
use tempfile::tempdir;

/// Runs a recursive local copy of `source/` into `dest`, optionally with
/// `--delete`, applying the supplied filter program. The trailing slash on the
/// source mirrors `rsync src/ dst/` so relative paths line up at the same root
/// in both trees, keeping the dest-tree assertions layout-stable.
fn run_transfer(source: &Path, dest: &Path, delete: bool, program: FilterProgram) {
    let mut source_arg = source.to_path_buf().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_arg, dest.to_path_buf().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan builds");

    let mut options = LocalCopyOptions::new()
        .recursive(true)
        .with_filter_program(Some(program));
    if delete {
        options = options.delete(true);
    }

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");
}

/// Recursively copies a directory tree (`cp -a`-like) to pre-populate a
/// destination for the `--delete` cases.
fn populate(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create dest dir");
    for entry in fs::read_dir(source).expect("read source dir") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            populate(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// `P` / `protect` keeps a matching destination file from being deleted by
/// `--delete`, even though the file is absent from the source (and would
/// otherwise be extraneous). This is the receiver-side guard.
///
/// Upstream capture (rsync 3.4.4), identical dest trees required:
/// ```text
/// rsync -r --delete --filter='P keep.log' src/ dst/
/// # dst/keep.log survives; dst/stale.log is deleted.
/// ```
///
/// upstream: `protect` sets `FILTRULE_RECEIVER_SIDE` (exclude.c:1204-1206); the
/// delete pass consults receiver-side rules and a matched protect rule keeps
/// the file (delete.c:delete_in_dir via name_is_excluded / check_filter).
#[test]
fn protect_keeps_extraneous_dest_file_from_delete() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");

    // Source has only `real.txt`; both `keep.log` and `stale.log` exist ONLY
    // in the destination, so both are extraneous under --delete.
    fs::create_dir_all(&source).expect("mkdir src");
    fs::write(source.join("real.txt"), b"payload").expect("write real.txt");

    populate(&source, &dest);
    fs::write(dest.join("keep.log"), b"protected").expect("write keep.log");
    fs::write(dest.join("stale.log"), b"extraneous").expect("write stale.log");

    let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::protect("keep.log"))])
        .expect("filter program compiles");
    run_transfer(&source, &dest, true, program);

    assert!(
        dest.join("keep.log").exists(),
        "`P keep.log` must protect the extraneous dest file from --delete",
    );
    assert!(
        !dest.join("stale.log").exists(),
        "an unprotected extraneous dest file must be deleted by --delete",
    );
    assert!(
        dest.join("real.txt").exists(),
        "the transferred source file must remain",
    );
}

/// `R` / `risk` cancels an earlier `protect`, re-exposing the matching
/// destination file to `--delete`. Rule order matters: with first-match-wins
/// the more specific `risk` must precede the broad `protect` so the risked file
/// is deleted while its siblings stay protected.
///
/// Upstream capture (rsync 3.4.4), identical dest trees required:
/// ```text
/// rsync -r --delete --filter='R secret.log' --filter='P *.log' src/ dst/
/// # dst/secret.log is deleted; dst/keep.log survives.
/// ```
///
/// upstream: `risk` sets `FILTRULE_INCLUDE | FILTRULE_RECEIVER_SIDE`
/// (exclude.c:1201-1206); `check_filter()` returns on the FIRST matching rule
/// (exclude.c:1058-1061), so a risk (include) matched before a protect
/// (exclude) yields "may delete" and the delete pass removes the file.
///
/// KNOWN DIVERGENCE (verified on Linux against rsync 3.4.3-149):
/// oc-rsync keeps `secret.log` here, whereas upstream deletes it. The engine's
/// protect/risk evaluator in
/// `crates/engine/src/local_copy/filter_program/segments.rs` iterates the whole
/// `protect_risk` chain and applies every match, so a later `P *.log`
/// (`outcome.protect()`) overwrites the earlier `R secret.log`
/// (`outcome.unprotect()`) - last-match-wins instead of upstream's
/// first-match-wins. The include/exclude loop in the same file already latches
/// on the first match via `transfer_decided()`; the protect/risk loop lacks the
/// equivalent guard.
///
/// This test asserts the UPSTREAM-CORRECT outcome and is `#[ignore]`d until the
/// evaluator is fixed to break on the first matching protect/risk rule. It must
/// not be changed to assert oc's current (wrong) last-match behaviour.
#[test]
fn risk_re_exposes_protected_dest_file_to_delete() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");

    // Source is empty of `.log` files; both logs are extraneous in the dest.
    fs::create_dir_all(&source).expect("mkdir src");
    fs::write(source.join("real.txt"), b"payload").expect("write real.txt");

    populate(&source, &dest);
    fs::write(dest.join("secret.log"), b"risked").expect("write secret.log");
    fs::write(dest.join("keep.log"), b"protected").expect("write keep.log");

    // `R secret.log` before `P *.log`: risk wins for secret.log (first match),
    // protect still guards every other *.log.
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::risk("secret.log")),
        FilterProgramEntry::Rule(FilterRule::protect("*.log")),
    ])
    .expect("filter program compiles");
    run_transfer(&source, &dest, true, program);

    assert!(
        !dest.join("secret.log").exists(),
        "`R secret.log` before `P *.log` must re-expose secret.log to --delete",
    );
    assert!(
        dest.join("keep.log").exists(),
        "`P *.log` must still protect keep.log from --delete",
    );
}

/// `H` / `hide` omits a matching source file from the transfer file list, so it
/// never reaches the destination. This is the sender-side exclude - unlike a
/// plain exclude it has no receiver-side effect, but the observable transfer
/// outcome (file absent from dest) is what upstream produces.
///
/// Upstream capture (rsync 3.4.4), identical dest trees required:
/// ```text
/// rsync -r --filter='H *.bak' src/ dst/
/// # dst/data.txt present; dst/data.bak absent.
/// ```
///
/// upstream: `hide` sets `FILTRULE_SENDER_SIDE` (exclude.c:1197-1199); the
/// sender's file-list build consults sender-side rules and drops the match.
#[test]
fn hide_omits_source_file_from_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");

    fs::create_dir_all(&source).expect("mkdir src");
    fs::write(source.join("data.txt"), b"kept").expect("write data.txt");
    fs::write(source.join("data.bak"), b"hidden").expect("write data.bak");

    let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::hide("*.bak"))])
        .expect("filter program compiles");
    run_transfer(&source, &dest, false, program);

    assert!(
        dest.join("data.txt").exists(),
        "a non-hidden source file must transfer",
    );
    assert!(
        !dest.join("data.bak").exists(),
        "`H *.bak` must hide data.bak from the sender file list",
    );
}

/// `S` / `show` cancels a broader `hide`, re-showing the matching source file
/// so it transfers. Rule order matters: with first-match-wins the specific
/// `show` must precede the broad `hide` so only the shown file survives while
/// its siblings stay hidden.
///
/// Upstream capture (rsync 3.4.4), identical dest trees required:
/// ```text
/// rsync -r --filter='S important.bak' --filter='H *.bak' src/ dst/
/// # dst/important.bak present; dst/scratch.bak absent.
/// ```
///
/// upstream: `show` sets `FILTRULE_INCLUDE | FILTRULE_SENDER_SIDE`
/// (exclude.c:1194-1199); a sender-side include matched before the hide exclude
/// keeps the file in the sender file list.
#[test]
fn show_re_includes_hidden_source_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");

    fs::create_dir_all(&source).expect("mkdir src");
    fs::write(source.join("important.bak"), b"shown").expect("write important.bak");
    fs::write(source.join("scratch.bak"), b"hidden").expect("write scratch.bak");

    // `S important.bak` before `H *.bak`: show wins for important.bak (first
    // match), hide still omits every other *.bak.
    let program = FilterProgram::new([
        FilterProgramEntry::Rule(FilterRule::show("important.bak")),
        FilterProgramEntry::Rule(FilterRule::hide("*.bak")),
    ])
    .expect("filter program compiles");
    run_transfer(&source, &dest, false, program);

    assert!(
        dest.join("important.bak").exists(),
        "`S important.bak` before `H *.bak` must re-show important.bak",
    );
    assert!(
        !dest.join("scratch.bak").exists(),
        "`H *.bak` must still hide scratch.bak",
    );
}

/// Side isolation: `H`/`S` (sender-side) must not affect the receiver delete
/// pass, and `P`/`R` (receiver-side) must not affect the sender file list.
/// This is the direct observable of the side split at exclude.c:1194-1207.
///
/// Upstream capture (rsync 3.4.4):
/// ```text
/// # hide does not protect from delete:
/// rsync -r --delete --filter='H gone.txt' src/ dst/   # dst/gone.txt deleted
/// # protect does not hide from transfer:
/// rsync -r --filter='P block.txt' src/ dst/           # dst/block.txt present
/// ```
///
/// upstream: send_rules() elides sender-side rules from the receiver and
/// receiver-side rules from the sender (exclude.c:1605-1613).
#[test]
fn side_modifiers_do_not_cross_over() {
    // Leg 1: hide is sender-side only -> no delete protection.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("mkdir src");
    fs::write(source.join("real.txt"), b"payload").expect("write real.txt");
    populate(&source, &dest);
    // `gone.txt` is extraneous in the dest; a sender-side hide must not save it.
    fs::write(dest.join("gone.txt"), b"extraneous").expect("write gone.txt");

    let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::hide("gone.txt"))])
        .expect("filter program compiles");
    run_transfer(&source, &dest, true, program);
    assert!(
        !dest.join("gone.txt").exists(),
        "`H gone.txt` is sender-side and must NOT protect gone.txt from --delete",
    );

    // Leg 2: protect is receiver-side only -> no effect on the sender file list.
    let temp2 = tempdir().expect("tempdir");
    let source2 = temp2.path().join("src");
    let dest2 = temp2.path().join("dst");
    fs::create_dir_all(&source2).expect("mkdir src2");
    fs::write(source2.join("block.txt"), b"transferred").expect("write block.txt");

    let program2 = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::protect("block.txt"))])
        .expect("filter program compiles");
    run_transfer(&source2, &dest2, false, program2);
    assert!(
        dest2.join("block.txt").exists(),
        "`P block.txt` is receiver-side and must NOT hide block.txt from transfer",
    );
}
