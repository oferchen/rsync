use super::common::*;
use super::*;

/// A local transfer APPLIES its `-M` values instead of refusing them.
///
/// upstream: options.c:3175-3182 appends `remote_options[]` to the argv of the
/// server the client starts, and a local copy still forks one (do_cmd ->
/// local_child -> child_main), so the child parses them normally. Upstream has
/// no local-transfer refusal for `-M` anywhere; oc used to invent one because
/// the values had no local consumer and would otherwise have been dropped
/// silently.
///
/// The log file is the load-bearing assertion: a test that only checked the
/// exit code and the destination would pass just as well if `-M` were silently
/// IGNORED, which is the failure mode the old refusal existed to prevent.
#[test]
fn remote_option_applies_to_a_local_transfer() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    let log = temp.path().join("applied.log");
    std::fs::write(&source, b"content").expect("write source");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--remote-option=--log-file={}", log.display())),
        source.into_os_string(),
        dest.clone().into_os_string(),
    ]);

    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(code, 0, "-M must not be refused on a local copy: {message}");
    assert_eq!(
        std::fs::read(&dest).expect("dest"),
        b"content",
        "the transfer itself must still run"
    );
    assert!(
        log.exists(),
        "the -M value must be APPLIED, not merely tolerated"
    );
}

/// Non-vacuity companion for [`remote_option_applies_to_a_local_transfer`]:
/// without `-M` the same copy leaves no log file, so that test's log-file
/// assertion is attributable to the folded option and nothing else.
#[test]
fn a_local_transfer_writes_no_log_file_without_the_remote_option() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    let log = temp.path().join("applied.log");
    std::fs::write(&source, b"content").expect("write source");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(!log.exists());
}
