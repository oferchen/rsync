/// A module with no `path` loads; it is refused only when selected.
///
/// upstream: rsync_module() replies `@ERROR: no path setting.` when a client
/// selects it (clientserver.c:877-881); lp_load() accepts the section.
#[test]
fn runtime_options_loads_a_module_without_path() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\ncomment = sample\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("a module without path must load");

    assert!(options.modules()[0].path.as_os_str().is_empty());
}
