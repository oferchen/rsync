#[test]
fn runtime_options_parse_module_definitions() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs,Documentation"),
        OsString::from("--module"),
        OsString::from("logs=/var/log"),
    ])
    .expect("parse modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs"));
    assert_eq!(modules[0].comment.as_deref(), Some("Documentation"));
    assert!(modules[0].bandwidth_limit().is_none());
    assert!(modules[0].bandwidth_burst().is_none());
    assert!(modules[0].refused_options().is_empty());
    assert!(modules[0].read_only());
    assert_eq!(modules[1].name, "logs");
    assert_eq!(modules[1].path, PathBuf::from("/var/log"));
    assert!(modules[1].comment.is_none());
    assert!(modules[1].bandwidth_limit().is_none());
    assert!(modules[1].bandwidth_burst().is_none());
    assert!(modules[1].refused_options().is_empty());
    assert!(modules[1].read_only());
}

