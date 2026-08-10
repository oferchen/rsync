#[test]
fn runtime_options_inline_module_uses_default_secrets_file() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let args = [
        OsString::from("--module"),
        OsString::from(format!(
            "secure={}{}auth users=alice",
            module_dir.display(),
            ';'
        )),
    ];

    let options =
        with_test_secrets_candidates(vec![secrets_path.clone()], || RuntimeOptions::parse(&args))
            .expect("parse inline module with default secrets");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "secure");
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

