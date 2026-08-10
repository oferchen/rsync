#[test]
fn first_existing_config_path_falls_back_to_legacy_candidate() {
    let dir = tempdir().expect("tempdir");
    let legacy = dir.path().join("legacy.conf");
    fs::write(&legacy, "# legacy").expect("write legacy");

    let missing = dir.path().join("missing.conf");
    let expected = legacy.as_os_str().to_os_string();
    let result = first_existing_config_path([missing.as_path(), legacy.as_path()]);

    assert_eq!(result, Some(expected));
}

