//! Regression test for `--only-write-batch` dry-run semantics.
//!
//! Upstream `testsuite/batch-mode.test` asserts that `--only-write-batch`
//! must NOT create the destination directory (upstream `main.c:1815-1816`
//! forces `dry_run = 1` when `write_batch < 0`).
//!
//! Mirrors the test fragment:
//!
//! ```sh
//! $RSYNC -av --only-write-batch=BATCH --exclude=foobar.baz \
//!     "$fromdir/" "$todir/missing/"
//! test -d "$todir/missing" \
//!     && test_fail "--only-write-batch should not have created destination dir"
//! ```
//!
//! upstream: `main.c:1815-1816`
//! ```c
//! if (write_batch < 0)
//!     dry_run = 1;
//! ```
//!
//! The test also exercises a daemon-style scenario by replaying the captured
//! batch back to a fresh destination, verifying that the batch body is
//! self-contained and replayable.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

/// `--only-write-batch=BATCH source/ dest/missing/` must:
///   1. Succeed (exit 0).
///   2. Create the BATCH file and BATCH.sh script.
///   3. NOT create the destination directory `dest/missing`.
#[test]
fn only_write_batch_does_not_create_destination_dir() {
    let test_dir = TestDir::new().expect("create test dir");

    let src = test_dir.mkdir("src").expect("create src");
    let dest = test_dir.mkdir("dest").expect("create dest");

    test_dir
        .write_file("src/file.txt", b"hello batch")
        .expect("write source file");
    test_dir
        .write_file("src/subdir/inner.txt", b"nested batch payload")
        .expect("write nested source file");

    let batch_path = test_dir.path().join("BATCH");
    let dest_missing = dest.join("missing");

    let mut cmd = RsyncCommand::new();
    cmd.arg("-a")
        .arg(format!("--only-write-batch={}", batch_path.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dest_missing.display()));
    let output = cmd.assert_success();

    assert!(
        batch_path.exists(),
        "batch file '{}' must be created by --only-write-batch",
        batch_path.display()
    );

    let script_path = test_dir.path().join("BATCH.sh");
    assert!(
        script_path.exists(),
        "replay script '{}' must be created by --only-write-batch",
        script_path.display()
    );

    assert!(
        !dest_missing.exists(),
        "--only-write-batch should not have created destination dir '{}' \
         (stdout: {})",
        dest_missing.display(),
        String::from_utf8_lossy(&output.stdout),
    );
}

/// `--only-write-batch` followed by `--read-batch` to a fresh destination
/// must reconstruct the source tree byte-for-byte, mirroring upstream
/// `testsuite/batch-mode.test`:
///
/// ```sh
/// $RSYNC -av --only-write-batch=BATCH ... "$fromdir/" "$todir/missing/"
/// runtest "--read-batch (only)" 'checkit "$RSYNC -av --read-batch=BATCH \"$todir\"" ...'
/// ```
#[test]
fn only_write_batch_then_read_batch_replays_source_tree() {
    let test_dir = TestDir::new().expect("create test dir");

    let src = test_dir.mkdir("src").expect("create src");
    let dest = test_dir.mkdir("dest").expect("create dest");
    let replay = test_dir.mkdir("replay").expect("create replay");

    test_dir
        .write_file("src/file.txt", b"hello batch")
        .expect("write source file");
    test_dir
        .write_file("src/subdir/inner.txt", b"nested batch payload")
        .expect("write nested source file");

    let batch_path = test_dir.path().join("BATCH");
    let dest_missing = dest.join("missing");

    let mut writer = RsyncCommand::new();
    writer
        .arg("-a")
        .arg(format!("--only-write-batch={}", batch_path.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dest_missing.display()));
    writer.assert_success();

    assert!(
        !dest_missing.exists(),
        "--only-write-batch must not create destination directory"
    );

    let mut reader = RsyncCommand::new();
    reader
        .arg("-a")
        .arg(format!("--read-batch={}", batch_path.display()))
        .arg(format!("{}/", replay.display()));
    reader.assert_success();

    assert!(
        replay.join("file.txt").exists(),
        "replay must reconstruct top-level file"
    );
    assert!(
        replay.join("subdir/inner.txt").exists(),
        "replay must reconstruct nested file"
    );

    let original = std::fs::read(src.join("file.txt")).expect("read source file");
    let replayed = std::fs::read(replay.join("file.txt")).expect("read replay file");
    assert_eq!(
        original, replayed,
        "replay must reproduce file contents byte-for-byte"
    );

    let original_nested =
        std::fs::read(src.join("subdir/inner.txt")).expect("read nested source file");
    let replayed_nested =
        std::fs::read(replay.join("subdir/inner.txt")).expect("read nested replay file");
    assert_eq!(
        original_nested, replayed_nested,
        "replay must reproduce nested file contents byte-for-byte"
    );
}
