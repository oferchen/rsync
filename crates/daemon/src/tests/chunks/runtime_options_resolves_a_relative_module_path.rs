// A relative module `path` is resolved against the daemon's current directory,
// not refused - including under `use chroot`.
//
// upstream: `normalize_path` (util1.c:1405-1426) opens with
// `if (*path != '/') { /* Make path absolute. */ ... }`, and `rsync_module()`
// routes every module path through it on both the chroot arm
// (clientserver.c:898-916) and the no-chroot arm (:916-918). There is no
// absolute-path requirement anywhere in upstream's config parser.
//
// oc previously refused this configuration outright, which is why the 3.5.0
// testsuite's daemon-* cells died before the daemon ever listened: the harness
// writes module paths relative to its scratch directory.
#[test]
fn runtime_options_resolves_a_relative_module_path() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = yes\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("a relative module path is resolved against the cwd, not refused");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let expected = std::env::current_dir()
        .expect("current directory")
        .join("data/docs");
    assert_eq!(modules[0].path, expected);
}

// Non-vacuity companion: resolution must not disturb a path that is already
// absolute. Without this, replacing the join with an unconditional rewrite -
// or dropping the check entirely and storing the raw value - would still
// satisfy the test above.
//
// upstream: `normalize_path`'s `*path != '/'` arm is the only one that
// rewrites; an absolute path is merely cleaned, and the bare root `/` is
// preserved verbatim (loadparm.c P_PATH).
#[test]
fn runtime_options_leaves_an_absolute_module_path_verbatim() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nuse chroot = yes\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("an absolute module path parses");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, Path::new("/srv/docs"));
}
