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

