#[test]
fn runtime_options_rejects_duplicate_use_chroot_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nuse chroot = yes\nuse chroot = no\n",
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate directive should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'use chroot' directive")
    );
}

