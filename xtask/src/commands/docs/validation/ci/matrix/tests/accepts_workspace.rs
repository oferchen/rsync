use super::*;

#[test]
fn validate_ci_cross_compile_matrix_accepts_workspace_configuration() {
    let workspace = crate::workspace::workspace_root().expect("workspace root");
    let branding = load_workspace_branding(&workspace).expect("branding");
    let mut failures = Vec::new();
    validate_ci_cross_compile_matrix(&workspace, &branding, &mut failures)
        .expect("validation succeeds");
    assert!(
        failures.is_empty(),
        "unexpected CI validation failures: {failures:?}"
    );
}
