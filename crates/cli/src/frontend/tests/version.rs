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

// upstream: rsync -VV outputs JSON capability information
#[test]
fn double_version_flag_renders_json() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("-VV")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let output = String::from_utf8(stdout).expect("valid UTF-8");
    assert!(
        output.starts_with('{'),
        "JSON output should start with '{{'"
    );
    assert!(output.contains("\"program\""), "should contain program key");
    assert!(output.contains("\"version\""), "should contain version key");
    assert!(output.contains("\"atimes\""), "should contain atimes key");
    assert!(output.contains("\"crtimes\""), "should contain crtimes key");
    assert!(output.contains("\"ACLs\""), "should contain ACLs key");
    assert!(output.contains("\"xattrs\""), "should contain xattrs key");
    assert!(
        output.contains("\"capabilities\""),
        "should contain capabilities section"
    );
    assert!(
        output.contains("\"optimizations\""),
        "should contain optimizations section"
    );
    assert!(
        output.contains("\"license\": \"GPLv3\""),
        "should contain GPLv3 license"
    );
}

// upstream: rsync -V -V is equivalent to -VV
#[test]
fn separate_version_flags_render_json() {
    let (code, stdout, stderr) =
        run_with_args([OsStr::new(RSYNC), OsStr::new("-V"), OsStr::new("-V")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let output = String::from_utf8(stdout).expect("valid UTF-8");
    assert!(
        output.starts_with('{'),
        "JSON output should start with '{{'"
    );
    assert!(output.contains("\"atimes\""), "should contain atimes key");
}

#[test]
fn double_version_json_matches_report() {
    let (code, stdout, _stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("-VV")]);

    assert_eq!(code, 0);

    let expected = VersionInfoReport::for_client_brand(Brand::Upstream).json();
    assert_eq!(stdout, expected.into_bytes());
}
