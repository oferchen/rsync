// A bare `include` in the global section is the P_LOCAL `include` filter
// parameter (daemon-parm.txt `Locals:` `include`, registered via daemon-parm.h
// in loadparm.c), NOT config-file inclusion - upstream reserves `&include` for
// pulling in another file (params.c). WHY this matters: misrouting a global
// `include = *.bak` to file inclusion would try to read `*.bak` as a config
// file (an error) instead of seeding every module's include-filter default.
// This test locks in that the value flows to `module_defaults.include` and is
// inherited by modules that declare no `include` of their own, exactly like
// the sibling `exclude` default.
#[test]
fn runtime_options_global_include_filter_default_inherited() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "include = *.bak\n\
         \n\
         [alpha]\n\
         path = /srv/alpha\n\
         \n\
         [beta]\n\
         path = /srv/beta\n\
         include = *.keep\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);

    // alpha declares no include of its own, so it inherits the global default.
    assert_eq!(modules[0].include(), &["*.bak".to_owned()]);
    // beta overrides (upstream: STRING params replace, not append), so the
    // global default is not merged in.
    assert_eq!(modules[1].include(), &["*.keep".to_owned()]);
}
