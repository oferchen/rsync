#[test]
fn runtime_options_default_secrets_path_updates_delegate_arguments() {
    let dir = tempdir().expect("config dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options =
        with_test_secrets_candidates(vec![secrets_path.clone()], || RuntimeOptions::parse(&[]))
            .expect("parse defaults with secrets override");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.into_os_string(),
        ]
    );
}

