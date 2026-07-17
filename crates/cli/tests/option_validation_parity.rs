//! Option-parsing/interaction parity with upstream rsync 3.4.4 (`options.c`).
//!
//! Each test pins an upstream validation or interaction rule so that oc-rsync
//! rejects, clamps, or accepts option combinations byte-for-byte the way the
//! reference implementation does.

use cli::test_utils::parse_args;
use core::client::DeleteMode;

// upstream: options.c:2182-2185,2215-2217 - `--max-delete` only caps the count
// and must never enable deletion. Promoting it silently deletes extraneous
// destination files (data loss).
#[test]
fn max_delete_alone_does_not_enable_deletion() {
    let args = parse_args(["oc-rsync", "--max-delete=5", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(args.delete_mode, DeleteMode::Disabled);
    assert_eq!(args.max_delete, Some("5".into()));
}

// With an explicit `--delete`, the cap is retained alongside the enabled mode.
#[test]
fn max_delete_with_delete_caps_and_enables() {
    let args = parse_args([
        "oc-rsync",
        "--delete",
        "--max-delete=5",
        "--dirs",
        "src",
        "dest",
    ])
    .unwrap();
    assert_eq!(args.delete_mode, DeleteMode::DuringDefault);
    assert_eq!(args.max_delete, Some("5".into()));
}

// upstream: options.c:2182-2185 - a negative `--max-delete` is clamped to a 0
// cap and parsing continues; it is not an error and does not enable deletion.
#[test]
fn negative_max_delete_is_accepted_and_does_not_enable_deletion() {
    let args = parse_args(["oc-rsync", "--max-delete=-1", "--dirs", "src", "dest"]).unwrap();
    assert_eq!(args.delete_mode, DeleteMode::Disabled);
    assert_eq!(args.max_delete, Some("-1".into()));
}

// upstream: options.c:724-725,2210 - `--delete-during`/`--del` and
// `--delete-delay` share the single `delete_during` term, so combining them is
// NOT a conflict.
#[test]
fn delete_during_and_delay_share_a_when_term() {
    let result = parse_args([
        "oc-rsync",
        "--dirs",
        "--del",
        "--delete-delay",
        "src",
        "dest",
    ]);
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
}

// upstream: options.c:2210-2213 - distinct WHEN phases conflict with the exact
// wording below.
#[test]
fn multiple_delete_when_phases_use_upstream_wording() {
    let err = parse_args([
        "oc-rsync",
        "--delete-before",
        "--delete-after",
        "--dirs",
        "src",
        "dest",
    ])
    .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    assert!(
        err.to_string()
            .contains("You may not combine multiple --delete-WHEN options."),
        "wrong wording: {err}"
    );
}

// upstream: options.c:2187-2203,2230-2234 - `--delete` needs a resolved
// xfer_dirs. `--files-from` sets xfer_dirs=1, so `--delete --files-from` is
// permitted even without `-r`/`-d`.
#[test]
fn delete_with_files_from_is_permitted_without_recursive() {
    let result = parse_args([
        "oc-rsync",
        "--delete",
        "--files-from=list.txt",
        "src",
        "dest",
    ]);
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
}

// upstream: options.c:2203 - `--list-only` also resolves xfer_dirs, permitting
// `--delete` without `-r`/`-d`.
#[test]
fn delete_with_list_only_is_permitted_without_recursive() {
    let result = parse_args(["oc-rsync", "--delete", "--list-only", "src"]);
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
}

// upstream: options.c:2230-2234 - without `-r`/`-d` (and no files-from/list-only
// to resolve xfer_dirs), `--delete` is rejected.
#[test]
fn delete_without_dirs_is_rejected() {
    let err = parse_args(["oc-rsync", "--delete", "src", "dest"]).unwrap_err();
    assert!(
        err.to_string()
            .contains("--delete does not work without --recursive (-r) or --dirs (-d)."),
        "wrong wording: {err}"
    );
}

// upstream: options.c:2126-2130 - `--fake-super` conflicts with `-XX`.
#[test]
fn fake_super_conflicts_with_double_x() {
    let err = parse_args(["oc-rsync", "--fake-super", "-XX", "src", "dest"]).unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    assert!(
        err.to_string().contains("--fake-super conflicts with -XX"),
        "wrong wording: {err}"
    );
}

// A single `-X` with `--fake-super` is fine.
#[test]
fn fake_super_with_single_x_is_accepted() {
    let result = parse_args(["oc-rsync", "--fake-super", "-X", "src", "dest"]);
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
}

// upstream: options.c:2158-2162 - `--read-batch` with `--files-from` is an error.
#[test]
fn read_batch_conflicts_with_files_from() {
    let err = parse_args([
        "oc-rsync",
        "--read-batch=b",
        "--files-from=list.txt",
        "src",
        "dest",
    ])
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("--read-batch cannot be used with --files-from"),
        "wrong wording: {err}"
    );
}

// upstream: options.c:2163-2167 - `--read-batch` with `--remove-source-files`.
#[test]
fn read_batch_conflicts_with_remove_source_files() {
    let err = parse_args([
        "oc-rsync",
        "--read-batch=b",
        "--remove-source-files",
        "src",
        "dest",
    ])
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("--read-batch cannot be used with --remove-source-files"),
        "wrong wording: {err}"
    );
}

// upstream: options.c:2299-2304 - a `--suffix` containing a slash is rejected.
#[test]
fn suffix_with_slash_is_rejected() {
    let err = parse_args(["oc-rsync", "--suffix=a/b", "src", "dest"]).unwrap_err();
    assert!(
        err.to_string()
            .contains("--suffix cannot contain slashes: a/b"),
        "wrong wording: {err}"
    );
}

// upstream: options.c:2328-2335 - an empty `--suffix` without `--backup-dir`.
#[test]
fn empty_suffix_without_backup_dir_is_rejected() {
    let err = parse_args(["oc-rsync", "--suffix=", "src", "dest"]).unwrap_err();
    assert!(
        err.to_string()
            .contains("--suffix cannot be empty without a --backup-dir"),
        "wrong wording: {err}"
    );
}

// An empty `--suffix` paired with `--backup-dir` is valid.
#[test]
fn empty_suffix_with_backup_dir_is_accepted() {
    let result = parse_args(["oc-rsync", "--suffix=", "--backup-dir=bak", "src", "dest"]);
    assert!(result.is_ok(), "unexpected error: {:?}", result.err());
}

// upstream: options.c:2296-2307 - `--suffix` alone does not enable backups.
#[test]
fn suffix_alone_does_not_enable_backup() {
    let args = parse_args(["oc-rsync", "--suffix=.bak", "src", "dest"]).unwrap();
    assert!(!args.backup, "--suffix must not imply --backup");
    assert_eq!(args.backup_suffix, Some(".bak".into()));
}
