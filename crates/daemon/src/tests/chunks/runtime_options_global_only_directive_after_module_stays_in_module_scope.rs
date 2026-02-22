#[test]
fn runtime_options_global_only_directive_after_module_stays_in_module_scope() {
    // In upstream rsync, once a [module] section starts, all subsequent
    // directives are scoped to that module. A global-only directive like
    // "reverse lookup" placed after a [module] header is treated as an
    // unknown per-module directive and silently ignored.
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "reverse lookup = no\n\
         \n\
         [mod]\n\
         path = /srv/mod\n\
         incoming chmod = a+r\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    // Module-level directive is applied correctly
    assert_eq!(modules[0].incoming_chmod(), Some("a+r"));

    // The global reverse lookup was set before the module
    assert!(!options.reverse_lookup());
}
