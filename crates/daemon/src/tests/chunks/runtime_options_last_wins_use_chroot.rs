/// A `use chroot` directive repeated inside one module section keeps its last
/// value instead of failing the load.
///
/// upstream: loadparm.c:379-470 do_parameter() has no duplicate check, so
/// rsync 3.4.4 starts and serves the module with `use chroot = no`.
#[test]
fn runtime_options_last_wins_use_chroot() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nuse chroot = yes\nuse chroot = no\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("repeated use chroot is accepted");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert!(!modules[0].use_chroot());
}
