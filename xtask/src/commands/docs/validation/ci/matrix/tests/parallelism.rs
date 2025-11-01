use super::*;

#[test]
fn validate_ci_cross_compile_matrix_requires_parallelism_settings() {
    let workspace = unique_workspace("xtask_docs_ci_parallelism");
    if workspace.exists() {
        fs::remove_dir_all(&workspace).expect("cleanup workspace");
    }

    write_manifest(&workspace);
    write_ci_file(
        &workspace,
        r#"name: CI

jobs:
  cross-compile:
    strategy:
      matrix:
        platform:
          - name: linux-x86_64
            enabled: true
            target: x86_64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: false
            generate_sbom: true
          - name: linux-aarch64
            enabled: true
            target: aarch64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: true
            generate_sbom: true
          - name: darwin-x86_64
            enabled: true
            target: x86_64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: darwin-aarch64
            enabled: true
            target: aarch64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: windows-x86_64
            enabled: true
            target: x86_64-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-x86
            enabled: false
            target: i686-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-aarch64
            enabled: false
            target: aarch64-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
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
    write_ci_file(
        &workspace,
        r#"name: CI

jobs:
  cross-compile:
    strategy:
      fail-fast: true
      max-parallel: 1
      matrix:
        platform:
          - name: linux-x86_64
            enabled: true
            target: x86_64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: false
            generate_sbom: true
          - name: linux-aarch64
            enabled: true
            target: aarch64-unknown-linux-gnu
            build_command: build
            build_daemon: true
            uses_zig: false
            needs_cross_gcc: true
            generate_sbom: true
          - name: darwin-x86_64
            enabled: true
            target: x86_64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: darwin-aarch64
            enabled: true
            target: aarch64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: true
          - name: windows-x86_64
            enabled: true
            target: x86_64-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-x86
            enabled: false
            target: i686-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
          - name: windows-aarch64
            enabled: false
            target: aarch64-pc-windows-msvc
            build_command: zigbuild
            build_daemon: false
            uses_zig: true
            needs_cross_gcc: false
            generate_sbom: false
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
