#[test]
fn runtime_options_module_definition_supports_escaped_commas() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs\\,archive,Project\\, Docs"),
    ])
    .expect("parse modules with escapes");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs,archive"));
    assert_eq!(modules[0].comment.as_deref(), Some("Project, Docs"));
    assert!(modules[0].read_only());
}

