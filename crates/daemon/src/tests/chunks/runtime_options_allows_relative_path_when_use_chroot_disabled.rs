#[test]
fn runtime_options_allows_relative_path_when_use_chroot_disabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = no\n",).expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    // The no-chroot arm resolves a relative `path` against the current
    // directory, exactly as the chroot arm does. upstream `rsync_module()`
    // routes BOTH arms through the same `normalize_path` call - the chroot arm
    // at clientserver.c:904 and this one at clientserver.c:915 - so the absence
    // of `use chroot` changes which directory the daemon serves FROM, never
    // whether the operator's spelling is made absolute.
    let expected = std::env::current_dir()
        .expect("current dir")
        .join("data/docs");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, expected);
    assert!(!modules[0].use_chroot());
}
