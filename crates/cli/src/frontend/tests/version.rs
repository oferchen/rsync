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

    let expected = VersionInfoReport::default().human_readable();
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

    let expected = VersionInfoReport::default().human_readable();
    assert_eq!(stdout, expected.into_bytes());
}
