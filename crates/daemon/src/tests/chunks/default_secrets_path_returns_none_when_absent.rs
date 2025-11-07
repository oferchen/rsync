#[test]
fn default_secrets_path_returns_none_when_absent() {
    let dir = tempdir().expect("tempdir");
    let primary = dir.path().join("missing-primary.secrets");
    let secondary = dir.path().join("missing-secondary.secrets");

    let result = with_test_secrets_candidates(vec![primary, secondary], || {
        default_secrets_path_if_present(Brand::Oc)
    });

    assert!(result.is_none());
}

