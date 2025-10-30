#[test]
fn runtime_options_module_definition_preserves_escaped_backslash() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("logs=/var/log\\\\files,Log share"),
    ])
    .expect("parse modules with escaped backslash");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, PathBuf::from("/var/log\\files"));
    assert_eq!(modules[0].comment.as_deref(), Some("Log share"));
    assert!(modules[0].read_only());
}

