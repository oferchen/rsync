use super::common::*;
use super::*;

#[test]
fn run_reports_invalid_chmod_specification() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--chmod=a+q"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(rendered.contains("failed to parse --chmod specification"));
}

#[test]
fn run_without_operands_emits_usage_for_oc_rsync() {
    let (code, stdout, stderr) = run_with_args([OsString::from(OC_RSYNC)]);

    assert_eq!(code, 1);

    let stdout_text = String::from_utf8(stdout).expect("stdout utf8");
    assert_eq!(
        stdout_text,
        render_missing_operands_stdout(ProgramName::OcRsync)
    );

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("syntax or usage error"));
    assert_contains_client_trailer(&stderr_text);
}

#[test]
fn run_without_operands_emits_usage_for_legacy_rsync() {
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

    assert_eq!(code, 1);

    let stdout_text = String::from_utf8(stdout).expect("stdout utf8");
    assert_eq!(
        stdout_text,
        render_missing_operands_stdout(ProgramName::Rsync)
    );

    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains("syntax or usage error"));
    assert_contains_client_trailer(&stderr_text);
}
