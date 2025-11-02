use super::*;

#[test]
fn validate_ci_cross_compile_matrix_detects_unexpected_entries() {
    let workspace = unique_workspace("xtask_docs_ci_unexpected");
    if workspace.exists() {
        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    write_manifest(&workspace);
    write_default_workflows(&workspace);
    write_workflow_file(
        &workspace,
        "build-linux.yml",
        r#"name: build-linux

on:
  workflow_call:

jobs:
  build:
    strategy:
      fail-fast: false
      max-parallel: 2
      matrix:
        platform:
          - name: linux-x86_64
            enabled: true
            runner: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: false
            package_linux: true
            generate_sbom: true
          - name: linux-aarch64
            enabled: true
            runner: ubuntu-latest
            target: aarch64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: true
            package_linux: true
            generate_sbom: true
          - name: freebsd-x86_64
            enabled: false
            runner: ubuntu-latest
            target: x86_64-unknown-freebsd
            build_command: build
            build_daemon: false
            uses_zig: false
            needs_cross_gcc: false
            package_linux: false
            generate_sbom: false
    runs-on: ${{ matrix.platform.runner }}
    steps:
      - run: echo ok
"#,
    );

    let branding = load_workspace_branding(&workspace).expect("branding");
    let mut failures = Vec::new();
    validate_ci_cross_compile_matrix(&workspace, &branding, &mut failures)
        .expect("validation completes");
    assert!(
        failures
            .iter()
            .any(|message| message.contains("unexpected cross-compilation entry 'freebsd-x86_64'")),
        "expected unexpected-entry failure, got {failures:?}"
    );

    fs::remove_dir_all(&workspace).expect("cleanup workspace");
}
