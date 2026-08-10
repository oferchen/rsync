//! Regression coverage for `--relative` intermediate directory mtimes.
//!
//! Mirrors the upstream rsync `testsuite/relative.test` check that intermediate
//! directories materialized by `-R` carry their source mtime, not the wall-clock
//! time. Upstream's generator finalizes each implied path component via
//! `make_path()` + `recv_generator()`; we replicate that in the local-copy
//! executor.
//!
//! # Upstream Reference
//!
//! - `generator.c::make_path()` walks the chain of implied dirs.
//! - `receiver.c::recv_generator()` applies source metadata to each.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use filetime::{FileTime, set_file_mtime};
use tempfile::tempdir;

/// Asserts the destination dir's mtime is within 2 seconds of the expected
/// source mtime. The window absorbs filesystem timestamp truncation
/// (e.g. HFS+ second granularity) while still catching wall-clock leakage.
fn assert_mtime_close(dest: &Path, expected: SystemTime, label: &str) {
    let meta = fs::metadata(dest).unwrap_or_else(|e| panic!("stat {}: {e}", dest.display()));
    let actual = meta
        .modified()
        .unwrap_or_else(|e| panic!("mtime {}: {e}", dest.display()));

    let delta = match actual.duration_since(expected) {
        Ok(d) => d,
        Err(e) => e.duration(),
    };
    assert!(
        delta <= Duration::from_secs(2),
        "{label}: dest mtime drifted by {delta:?} from source (dest={}, expected={expected:?}, actual={actual:?})",
        dest.display()
    );
}

/// `-R` against `<src>/./down/3/deep/` must stamp each intermediate dir on the
/// destination with the source dir's mtime, not the wall-clock time the
/// receiver materialized it.
#[test]
fn relative_preserves_intermediate_directory_mtimes() {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");

    let deep = from.join("down").join("3").join("deep");
    fs::create_dir_all(&deep).expect("create source tree");
    fs::create_dir_all(&to).expect("create destination root");

    // Drop a payload so the deep directory has work to do beyond mkdir.
    fs::write(deep.join("payload"), b"relative payload").expect("write payload");

    // Backdate each source dir to a distinct mtime well in the past so the
    // executor cannot accidentally pass by inheriting current time.
    let now = SystemTime::now();
    let mtime_down = now - Duration::from_secs(7200);
    let mtime_down_3 = now - Duration::from_secs(5400);
    let mtime_deep = now - Duration::from_secs(3600);

    set_file_mtime(from.join("down"), FileTime::from_system_time(mtime_down))
        .expect("backdate down");
    set_file_mtime(
        from.join("down").join("3"),
        FileTime::from_system_time(mtime_down_3),
    )
    .expect("backdate down/3");
    set_file_mtime(
        from.join("down").join("3").join("deep"),
        FileTime::from_system_time(mtime_deep),
    )
    .expect("backdate down/3/deep");

    // Operand carries the `/./` marker that anchors the relative chain at
    // `down/3/deep`. Mirrors upstream `rsync -avR ./down/3/deep $todir` from
    // testsuite/relative.test.
    let mut operand = from.clone();
    operand.push(".");
    operand.push("down");
    operand.push("3");
    operand.push("deep");

    let operands = vec![operand.into_os_string(), to.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .times(true)
        .permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("relative copy succeeds");

    // Sanity: payload survived the trip.
    assert!(
        to.join("down")
            .join("3")
            .join("deep")
            .join("payload")
            .is_file(),
        "payload must have been copied"
    );

    // Each intermediate dir must carry its source's mtime, not the wall-clock
    // moment the receiver invoked create_dir_all().
    assert_mtime_close(&to.join("down"), mtime_down, "down");
    assert_mtime_close(&to.join("down").join("3"), mtime_down_3, "down/3");
    assert_mtime_close(
        &to.join("down").join("3").join("deep"),
        mtime_deep,
        "down/3/deep",
    );
}
