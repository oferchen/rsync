#[test]
fn default_secrets_path_falls_back_to_secondary_candidate() {
    let dir = tempdir().expect("tempdir");
    let fallback = dir.path().join("fallback.secrets");
    fs::write(&fallback, "bob:password\n").expect("write fallback");

    let missing = dir.path().join("missing.secrets");
    let result = with_test_secrets_candidates(vec![missing, fallback.clone()], || {
        default_secrets_path_if_present(Brand::Oc)
    });

    assert_eq!(result, Some(fallback.into_os_string()));
}

