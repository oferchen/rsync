//! `--iconv` strict-skip for a top-level source operand whose own name
//! cannot be transcoded to the remote charset.
//!
//! Upstream rsync transcodes every file-list entry through
//! `iconvbufs(ic_send, ..., ICB_INIT)` in `send_file1()`. `ICB_INIT` is the
//! strict mode: a name the peer charset cannot represent makes the call return
//! `< 0`, whereupon upstream sets `io_error |= IOERR_GENERAL`, prints a
//! `cannot convert filename` diagnostic, and `return NULL`s so the entry never
//! enters the file list and is never transferred; the run finishes
//! `RERR_PARTIAL` (exit 23).
//!
//! The recursive walk already drops unconvertible *children* in the planner,
//! but a top-level named operand's own leaf name is not seen by the planner.
//! Before this was gated, oc transcoded the operand's name with the lossy
//! (pass-through) converter and WROTE the file at the destination with exit 0 -
//! diverging from upstream, which skips it and exits 23. These tests pin the
//! skip semantics for the operand boundary.
//!
//! The unconvertible fixture uses `あ` (U+3042), which is valid UTF-8 (so the
//! SOURCE is creatable on every filesystem, including APFS/NTFS) yet has no
//! ISO-8859-1 representation, so no destination file is ever written and the
//! test is platform-agnostic.
//!
//! # Upstream Reference
//!
//! - `flist.c:2010-2024` `send_file1()` - strict `ic_send` name conversion,
//!   `io_error |= IOERR_GENERAL`, and `return NULL` on failure.
//! - `main.c:1412` - `exit_cleanup(RERR_PARTIAL)` after `io_error`.
#![cfg(feature = "iconv")]

use std::fs;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use protocol::iconv::FilenameConverter;
use tempfile::tempdir;

/// A single top-level file operand whose name (`あ.txt`) cannot be transcoded
/// UTF-8 -> ISO-8859-1 is skipped, exactly as upstream `send_file1()` drops it:
/// the local copy finishes with exit 23 (`RERR_PARTIAL`) and no destination
/// file is created.
///
/// Reverting the operand-boundary gate in `process_single_source` makes this
/// fail: the pre-fix path transcodes the name with the lossy converter and
/// writes `あ.txt` to the destination with exit 0.
#[test]
fn top_level_unconvertible_file_operand_is_skipped_with_exit_23() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // "あ.txt" is valid UTF-8 but unrepresentable in ISO-8859-1.
    fs::write(source.join("あ.txt"), b"payload").expect("write source");

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
    let operands = vec![
        source.join("あ.txt").into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_iconv(Some(converter));

    let err = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("unconvertible operand must fail, not silently write it");
    assert_eq!(
        err.exit_code(),
        23,
        "upstream exits RERR_PARTIAL (23) on an unconvertible operand"
    );

    let dest_names: Vec<_> = fs::read_dir(&dest)
        .expect("read dest")
        .map(|e| e.expect("dirent").file_name())
        .collect();
    assert!(
        dest_names.is_empty(),
        "no destination file may be written for a skipped operand; got {dest_names:?}"
    );
}

/// Non-vacuity companion: an ASCII operand converts cleanly (ASCII is a subset
/// of ISO-8859-1), so the same `--iconv` run transfers it and exits 0. This
/// proves the skip gate fires on the conversion failure, not on the mere
/// presence of a converter.
#[test]
fn top_level_convertible_file_operand_still_transfers() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"payload").expect("write source");

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
    let operands = vec![
        source.join("keep.txt").into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_iconv(Some(converter));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("convertible operand must transfer");

    let copied = fs::read(dest.join("keep.txt")).expect("read dest");
    assert_eq!(copied, b"payload");
}

/// A directory whose payload includes both a convertible and an unconvertible
/// operand copies the convertible one and skips the other, finishing exit 23 -
/// the mixed case that isolates per-operand skipping from a blanket abort.
#[test]
fn mixed_operands_transfer_convertible_and_skip_unconvertible() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"good").expect("write good");
    fs::write(source.join("あ.txt"), b"bad").expect("write bad");

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
    let operands = vec![
        source.join("keep.txt").into_os_string(),
        source.join("あ.txt").into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_iconv(Some(converter));

    let err = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("the unconvertible operand must make the run exit 23");
    assert_eq!(err.exit_code(), 23);

    // The convertible operand still landed; the unconvertible one did not.
    let dest_names: Vec<_> = fs::read_dir(&dest)
        .expect("read dest")
        .map(|e| {
            e.expect("dirent")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        dest_names,
        vec!["keep.txt".to_string()],
        "got {dest_names:?}"
    );
}
