use super::common::*;
use super::*;

#[test]
fn oc_short_version_flag_renders_oc_banner() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("-V")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default()
        .with_client_brand(Brand::Oc)
        .human_readable();
    assert_eq!(stdout, expected.into_bytes());
}
