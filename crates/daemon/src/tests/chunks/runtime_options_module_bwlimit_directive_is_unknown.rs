/// A per-module `bwlimit` directive does not exist in upstream rsyncd.conf, so
/// it is treated as an unknown per-module directive: warned and ignored, never
/// applied to the module, and never a parse error.
///
/// upstream: daemon-parm.txt has no `bwlimit` module parameter (it is only the
/// daemon command-line `--bwlimit`, options.c:862). loadparm.c's do_parameter
/// reports and skips a name it does not recognise inside a module section.
#[test]
fn runtime_options_module_bwlimit_directive_is_unknown() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 100\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("an unknown per-module directive is ignored, not a hard error");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "docs");
    // No module carries its own bwlimit cap, and the daemon-wide limit (only
    // ever set by the `--bwlimit` CLI flag or a global `bwlimit` directive) is
    // untouched by a stray module-section directive.
    assert!(options.bandwidth_limit().is_none());
}
