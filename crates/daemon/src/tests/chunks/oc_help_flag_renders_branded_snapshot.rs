#[test]
fn oc_help_flag_renders_branded_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC_D), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::OcRsyncd);
    assert_eq!(stdout, expected.into_bytes());
}

