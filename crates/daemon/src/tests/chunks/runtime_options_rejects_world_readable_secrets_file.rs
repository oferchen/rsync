#[cfg(unix)]
#[test]
fn runtime_options_rejects_world_readable_secrets_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o644)).expect("chmod secrets");

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("world-readable secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("must not be other-accessible")
    );
}

/// A group-readable (0640) secrets file is accepted, matching upstream
/// authenticate.c:120 check_secret() which rejects only OTHER access
/// ((st.st_mode & 06) != 0). Group access is permitted.
#[cfg(unix)]
#[test]
fn runtime_options_accepts_group_readable_secrets_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o640)).expect("chmod secrets");

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("group-readable secrets file should be accepted");
}

/// An other-writable (0602) secrets file is rejected: 0o6 catches OTHER
/// write even without OTHER read, matching upstream's `& 06` mask.
#[cfg(unix)]
#[test]
fn runtime_options_rejects_other_writable_secrets_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o602)).expect("chmod secrets");

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("other-writable secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("must not be other-accessible")
    );
}

