use super::*;

#[test]
fn validate_ci_cross_compile_matrix_detects_missing_entries() {
    let workspace = unique_workspace("xtask_docs_ci_validate");
    if workspace.exists() {
        fs::remove_dir_all(&workspace).expect("cleanup stale workspace");
    }

    write_manifest(&workspace);
    write_default_workflows(&workspace);
    write_workflow_file(
        &workspace,
        "build-windows.yml",
        r#"name: build-windows

on:
  workflow_call:

jobs:
  build:
    strategy:
      fail-fast: false
      max-parallel: 2
      matrix:
        platform:
          - name: windows-aarch64
            enabled: false
            runner: windows-latest
            target: aarch64-pc-windows-msvc
            build_command: build
            build_daemon: false
            uses_zig: false
            needs_cross_gcc: false
            package_linux: false
            generate_sbom: false
          - name: windows-x86
            enabled: false
            runner: windows-latest
            target: i686-pc-windows-msvc
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

    let branding = load_workspace_branding(&workspace).expect("load branding");
    let mut failures = Vec::new();
    validate_ci_cross_compile_matrix(&workspace, &branding, &mut failures)
        .expect("validation completes");
    assert!(
        failures
            .iter()
            .any(|message| message.contains("windows-x86_64")),
    );

    fs::remove_dir_all(&workspace).expect("cleanup workspace");
}
