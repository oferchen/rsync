use super::common::*;
use super::*;

#[test]
fn version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_client_brand(Brand::Upstream).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_version_flag_renders_oc_banner() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_client_brand(Brand::Oc).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn short_version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("-V")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_client_brand(Brand::Upstream).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn version_flag_ignores_additional_operands() {
    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("--version"),
        OsStr::new("source"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_client_brand(Brand::Upstream).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn short_version_flag_ignores_additional_operands() {
    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("-V"),
        OsStr::new("source"),
        OsStr::new("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_client_brand(Brand::Upstream).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn double_v_renders_json_output() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("-VV")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_client_brand(Brand::Upstream).machine_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn double_v_json_contains_atimes_true() {
    // upstream testsuite/atimes.test requires:
    //   $RSYNC -VV | grep '"atimes": true'
    let (code, stdout, _) = run_with_args([OsStr::new(RSYNC), OsStr::new("-VV")]);

    assert_eq!(code, 0);
    let output = String::from_utf8_lossy(&stdout);
    assert!(
        output.contains("\"atimes\": true"),
        "-VV JSON must contain '\"atimes\": true' for upstream testsuite compatibility"
    );
}
