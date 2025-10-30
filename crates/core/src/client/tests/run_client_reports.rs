use super::prelude::*;


#[test]
fn run_client_reports_missing_operands() {
    let config = ClientConfig::builder().build();
    let error = run_client(config).expect_err("missing operands should error");

    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("missing source operands"));
    assert!(
        rendered.contains(&format!("[client={}]", RUST_VERSION)),
        "expected missing operands error to include client trailer"
    );
}

