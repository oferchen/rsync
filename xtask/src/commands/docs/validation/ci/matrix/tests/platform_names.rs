use super::*;
use std::collections::BTreeSet;

#[test]
fn collect_matrix_platform_names_extracts_expected_entries() {
    let section = r#"  build:
    strategy:
      matrix:
        platform:
          - name: linux-x86_64
            target: x86_64-unknown-linux-gnu
          - name: linux-aarch64
            target: aarch64-unknown-linux-gnu
        include:
          - name: ignored
            something: else
"#;

    let names = collect_matrix_platform_names(section);
    assert_eq!(
        names,
        vec![String::from("linux-x86_64"), String::from("linux-aarch64"),],
    );
}

#[test]
fn check_for_unexpected_matrix_entries_reports_all_issues() {
    let names = vec![
        String::from("linux-x86_64"),
        String::from("linux-x86_64"),
        String::from("windows-x86_64"),
        String::from("unexpected"),
    ];
    let expected_entries = BTreeSet::from([
        String::from("linux-x86_64"),
        String::from("linux-aarch64"),
        String::from("windows-x86_64"),
    ]);
    let mut failures = Vec::new();

    check_for_unexpected_matrix_entries(
        ".github/workflows/cross-compile.yml",
        &names,
        &expected_entries,
        &mut failures,
    );

    assert_eq!(
        failures,
        vec![
            String::from(
                ".github/workflows/cross-compile.yml: duplicate cross-compilation entry 'linux-x86_64'",
            ),
            String::from(
                ".github/workflows/cross-compile.yml: unexpected cross-compilation entry 'unexpected'",
            ),
            String::from(
                ".github/workflows/cross-compile.yml: missing cross-compilation entry 'linux-aarch64'",
            ),
        ],
    );
}
