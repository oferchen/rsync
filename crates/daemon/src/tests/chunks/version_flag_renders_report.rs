#[test]
fn version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_daemon_brand(Brand::Upstream).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

