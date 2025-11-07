#[test]
fn advertised_capability_lines_report_modules_without_auth() {
    let module = ModuleRuntime::from(base_module("docs"));

    assert_eq!(
        advertised_capability_lines(&[module]),
        vec![String::from("modules")]
    );
}

