#[test]
fn default_secrets_path_prefers_primary_candidate() {
    let dir = tempdir().expect("tempdir");
    let primary = dir.path().join("primary.secrets");
    let fallback = dir.path().join("fallback.secrets");
    fs::write(&primary, "alice:password\n").expect("write primary");
    fs::write(&fallback, "bob:password\n").expect("write fallback");

    let result = with_test_secrets_candidates(vec![primary.clone(), fallback], || {
        default_secrets_path_if_present(Brand::Oc)
    });

    assert_eq!(result, Some(primary.into_os_string()));
}

