// chroot(2) does not exist on Windows; the daemon's `use chroot` enforcement
// is gated on `cfg(unix)` in module_parsing.rs and module_definition/finish.rs,
// so the rejection this test exercises is a POSIX-only behaviour.
#[cfg(unix)]
#[test]
fn runtime_options_rejects_relative_path_with_chroot_enabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = yes\n",).expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("relative path with chroot should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("requires an absolute path when 'use chroot' is enabled")
    );
}

