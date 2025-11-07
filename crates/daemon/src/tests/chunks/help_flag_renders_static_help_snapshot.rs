#[test]
fn help_flag_renders_static_help_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::Rsyncd);
    assert_eq!(stdout, expected.into_bytes());
}

