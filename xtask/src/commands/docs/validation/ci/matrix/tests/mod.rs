use super::*;
use crate::test_support;
use crate::workspace::load_workspace_branding;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn write_manifest(workspace: &Path) {
    if !workspace.exists() {
        fs::create_dir_all(workspace).expect("create workspace root");
    }

    let manifest = test_support::manifest_snippet();
    fs::write(workspace.join("Cargo.toml"), manifest).expect("write manifest");
}

fn write_workflow_file(workspace: &Path, name: &str, contents: &str) {
    let workflows = workspace.join(".github").join("workflows");
    fs::create_dir_all(&workflows).expect("create workflows");
    fs::write(workflows.join(name), contents).expect("write workflow");
}

fn write_default_workflows(workspace: &Path) {
    write_workflow_file(workspace, "cross-compile.yml", DEFAULT_AGGREGATOR_WORKFLOW);
    write_workflow_file(workspace, "build-linux.yml", DEFAULT_LINUX_WORKFLOW);
    write_workflow_file(workspace, "build-macos.yml", DEFAULT_MACOS_WORKFLOW);
    write_workflow_file(workspace, "build-windows.yml", DEFAULT_WINDOWS_WORKFLOW);
}

fn unique_workspace(prefix: &str) -> std::path::PathBuf {
    let unique_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}_{unique_suffix}"))
}

const DEFAULT_AGGREGATOR_WORKFLOW: &str = r#"name: cross-compile

on:
  workflow_call:

permissions:
  contents: read

jobs:
  linux:
    uses: ./.github/workflows/build-linux.yml
    secrets: inherit

  macos:
    uses: ./.github/workflows/build-macos.yml
    secrets: inherit

  windows:
    uses: ./.github/workflows/build-windows.yml
    secrets: inherit
"#;

const DEFAULT_LINUX_WORKFLOW: &str = r#"name: build-linux

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
    runs-on: ${{ matrix.platform.runner }}
    steps:
      - run: echo ok
"#;

const DEFAULT_MACOS_WORKFLOW: &str = r#"name: build-macos

on:
  workflow_call:

jobs:
  build:
    strategy:
      fail-fast: false
      max-parallel: 2
      matrix:
        platform:
          - name: darwin-x86_64
            enabled: true
            runner: macos-13
            target: x86_64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            package_linux: false
            generate_sbom: true
          - name: darwin-aarch64
            enabled: true
            runner: macos-14
            target: aarch64-apple-darwin
            build_command: zigbuild
            build_daemon: true
            uses_zig: true
            needs_cross_gcc: false
            package_linux: false
            generate_sbom: true
    runs-on: ${{ matrix.platform.runner }}
    steps:
      - run: echo ok
"#;

const DEFAULT_WINDOWS_WORKFLOW: &str = r#"name: build-windows

on:
  workflow_call:

jobs:
  build:
    strategy:
      fail-fast: false
      max-parallel: 3
      matrix:
        platform:
          - name: windows-x86_64
            enabled: false
            runner: windows-latest
            target: x86_64-pc-windows-msvc
            build_command: build
            build_daemon: false
            uses_zig: false
            needs_cross_gcc: false
            package_linux: false
            generate_sbom: false
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
"#;

// mod accepts_workspace;
mod duplicate_entries;
mod extract;
mod mismatched_fields;
mod missing_entries;
mod parallelism;
mod platform_names;
mod unexpected_entries;
