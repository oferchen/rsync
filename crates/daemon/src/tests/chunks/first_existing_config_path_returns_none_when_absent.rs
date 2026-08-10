#[test]
fn first_existing_config_path_returns_none_when_absent() {
    let dir = tempdir().expect("tempdir");
    let missing_primary = dir.path().join("missing-primary.conf");
    let missing_legacy = dir.path().join("missing-legacy.conf");
    let result = first_existing_config_path([missing_primary.as_path(), missing_legacy.as_path()]);

    assert!(result.is_none());
}

