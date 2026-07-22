//! Integration tests for `--debug=BACKUP` producer emissions on the
//! local-copy executor path.
//!
//! Confirms the upstream wire shapes from `backup.c` materialise through the
//! actual `LocalCopyPlan::execute_with_options` flow when `--backup` is
//! enabled, and that level-0 gating suppresses them.
//!
//! # Upstream Reference
//!
//! - `backup.c:200-207` - `"make_backup: HLINK %s successful.\n"` fires
//!   on the success branch of `do_link_at(from, to)` inside `link_or_rename`,
//!   which is the strategy oc-rsync's local-copy executor exercises by
//!   default when `--backup` is on and the destination shares a
//!   filesystem with the backup location.

use std::ffi::OsString;
use std::fs;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use tempfile::tempdir;

/// Collects every BACKUP debug emission since the last drain.
fn backup_debug_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Backup,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// Drives a full local-copy that overwrites an existing destination with
/// `--backup --suffix=~` and confirms the HLINK debug line fires at
/// level 1 with upstream's exact wording.
///
/// upstream: backup.c:200-207 - `DEBUG_GTE(BACKUP, 1)` on the HLINK
/// success branch of `link_or_rename`, which upstream tries before RENAME
/// whenever the caller doesn't prefer a rename outright.
#[test]
fn local_copy_backup_emits_hlink_debug_line() {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.backup = 1;
    init(cfg);
    let _ = drain_events();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("file.txt"), b"updated payload").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"original payload").expect("write dest");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .backup(true)
        .with_backup_suffix(Some(OsString::from("~")));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let expected = format!("make_backup: HLINK {} successful.", existing.display());
    let messages = backup_debug_messages();
    assert!(
        messages.iter().any(|m| m == &expected),
        "expected upstream-format BACKUP,1 HLINK line {expected:?}; got {messages:?}"
    );

    assert!(
        dest_root.join("file.txt~").exists(),
        "backup file should be written next to destination"
    );
}

/// Same scenario as above but with `--debug=BACKUP` disabled. No BACKUP
/// debug lines should fire even though the backup still happens.
#[test]
fn backup_debug_level_zero_suppresses_emission() {
    init(VerbosityConfig::default());
    let _ = drain_events();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("file.txt"), b"updated payload").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("file.txt"), b"original payload").expect("write dest");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .backup(true)
        .with_backup_suffix(Some(OsString::from("~")));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let messages = backup_debug_messages();
    assert!(
        messages.is_empty(),
        "level 0 must suppress BACKUP debug emissions; got {messages:?}"
    );

    assert!(
        dest_root.join("file.txt~").exists(),
        "backup should still occur regardless of debug flag"
    );
}
