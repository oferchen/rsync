use super::*;

#[test]
fn validate_ci_cross_compile_matrix_requires_parallelism_settings() {
    let workspace = unique_workspace("xtask_docs_ci_parallelism");
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
            .any(|message| message.contains("max-parallel")),
        "expected max-parallel validation failure, got {failures:?}"
    );
    assert!(
        failures.iter().any(|message| message.contains("fail-fast")),
        "expected fail-fast validation failure, got {failures:?}"
    );

    fs::remove_dir_all(&workspace).expect("cleanup workspace");
}

#[test]
fn validate_ci_cross_compile_matrix_rejects_serial_parallelism() {
    let workspace = unique_workspace("xtask_docs_ci_serial");
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
      fail-fast: true
      max-parallel: 1
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
            .any(|message| message.contains("max-parallel greater than 1")),
        "expected max-parallel range failure, got {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|message| message.contains("disable fail-fast")),
        "expected fail-fast requirement failure, got {failures:?}"
    );

    fs::remove_dir_all(&workspace).expect("cleanup workspace");
}
