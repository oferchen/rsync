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
