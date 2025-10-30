#[test]
fn first_existing_config_path_prefers_primary_candidate() {
    let dir = tempdir().expect("tempdir");
    let primary = dir.path().join("primary.conf");
    let legacy = dir.path().join("legacy.conf");
    fs::write(&primary, "# primary").expect("write primary");
    fs::write(&legacy, "# legacy").expect("write legacy");

    let expected = primary.as_os_str().to_os_string();
    let result = first_existing_config_path([primary.as_path(), legacy.as_path()]);

    assert_eq!(result, Some(expected));
}

