#[test]
fn oc_version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC_D), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_daemon_brand(Brand::Oc).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

