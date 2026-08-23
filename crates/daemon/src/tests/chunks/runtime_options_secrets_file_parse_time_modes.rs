/// Config parsing stores `secrets file` without judging its mode, exactly as
/// upstream's params.c does. The permission rules belong to `check_secret()`
/// (authenticate.c:168-181), which applies them per-authentication behind
/// `lp_strict_modes(module)` - see the `verify_secret_*` tests. Rejecting here
/// would make `strict modes = no` unhonourable, because the daemon would refuse
/// to load the config instead of serving the file the operator opted into.
#[cfg(unix)]
#[test]
fn runtime_options_accepts_world_readable_secrets_file_at_parse_time() {
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

    RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("a world-readable secrets file must not fail config parsing");
}

/// A group-readable (0640) secrets file is accepted at parse time, and stays
/// accepted at auth time too: `check_secret()` rejects only OTHER access
/// (`(st.st_mode & 06) != 0`).
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

/// Other-writable (0602) is likewise a parse-time non-event. It IS rejected at
/// auth time under `strict modes = yes`; that half is pinned by
/// `verify_secret_rejects_other_accessible_when_strict_modes_enabled`.
#[cfg(unix)]
#[test]
fn runtime_options_accepts_other_writable_secrets_file_at_parse_time() {
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

    RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("other-writable secrets file should not fail config parsing");
}

/// Non-vacuity companion for the three acceptances above: the parse-time
/// validator is still wired and still rejects. Without this, dropping the whole
/// validator would leave every acceptance test green.
#[test]
fn runtime_options_rejects_a_secrets_file_that_is_not_a_regular_file() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets-dir");
    fs::create_dir(&secrets_path).expect("secrets dir");

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
    .expect_err("a directory is not a usable secrets file");

    assert!(
        error
            .message()
            .to_string()
            .contains("must be a regular file")
    );
}
