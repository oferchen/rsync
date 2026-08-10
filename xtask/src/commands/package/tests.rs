#[cfg(all(test, target_os = "linux"))]
use super::build::{resolve_cross_compiler_for_tests, resolve_tarball_cross_compilers_for_tests};

#[cfg(all(test, target_os = "linux"))]
use super::tarball::{TarballPlatform, TarballSpec};

use super::{DIST_PROFILE, PackageOptions, execute};
use crate::error::TaskError;
use crate::util::test_env::EnvGuard;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

fn workspace_root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    })
}

type ScopedEnv = EnvGuard;

fn scoped_env(keys: &[&'static str]) -> ScopedEnv {
    let mut guard = EnvGuard::new();
    for key in keys {
        guard.track(key);
    }
    guard
}

fn set_os(env: &mut ScopedEnv, key: &'static str, value: &OsStr) {
    env.set(key, value);
}

fn set_str(env: &mut ScopedEnv, key: &'static str, value: &str) {
    set_os(env, key, OsStr::new(value));
}

#[test]
fn execute_with_no_targets_returns_success() {
    execute(
        workspace_root(),
        PackageOptions {
            build_deb: false,
            build_rpm: false,
            build_tarball: false,
            tarball_target: None,
            deb_variant: None,
            profile: None,
        },
    )
    .expect("execution succeeds when nothing to build");
}

#[test]
fn execute_reports_missing_cargo_deb_tool() {
    let mut env = scoped_env(&[
        "OC_RSYNC_PACKAGE_SKIP_BUILD",
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
    ]);
    set_str(&mut env, "OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    set_str(&mut env, "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "cargo deb");
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: true,
            build_rpm: false,
            build_tarball: false,
            tarball_target: None,
            deb_variant: None,
            profile: Some(OsString::from(DIST_PROFILE)),
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TaskError::ToolMissing(message) if message.contains("cargo deb")
    ));
}

#[test]
fn execute_reports_missing_cargo_rpm_tool() {
    let mut env = scoped_env(&[
        "OC_RSYNC_PACKAGE_SKIP_BUILD",
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "PATH",
    ]);
    set_str(&mut env, "OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    set_str(
        &mut env,
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "cargo rpm build",
    );
    let (fake_rpmbuild_dir, _fake_rpmbuild) = fake_rpmbuild_path();
    let mut path_entries = vec![fake_rpmbuild_dir.path().to_path_buf()];
    if let Some(existing) = env::var_os("PATH") {
        path_entries.extend(env::split_paths(&existing));
    }
    let joined_path = env::join_paths(path_entries).expect("compose PATH with fake rpmbuild");
    set_os(&mut env, "PATH", joined_path.as_os_str());
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: false,
            build_rpm: true,
            build_tarball: false,
            tarball_target: None,
            deb_variant: None,
            profile: Some(OsString::from("debug")),
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TaskError::ToolMissing(message) if message.contains("cargo rpm build")
    ));
}

#[test]
fn execute_reports_missing_rpmbuild_tool() {
    let mut env = scoped_env(&[
        "OC_RSYNC_PACKAGE_SKIP_BUILD",
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
    ]);
    set_str(&mut env, "OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    set_str(&mut env, "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "rpmbuild");
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: false,
            build_rpm: true,
            build_tarball: false,
            tarball_target: None,
            deb_variant: None,
            profile: Some(OsString::from(DIST_PROFILE)),
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TaskError::ToolMissing(message) if message.contains("rpmbuild")
    ));
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn execute_reports_missing_cross_compiler() {
    let mut env = scoped_env(&["OC_RSYNC_FORCE_MISSING_CARGO_TOOLS"]);
    set_str(
        &mut env,
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "aarch64-linux-gnu-gcc,zig",
    );

    let error = resolve_cross_compiler_for_tests(workspace_root(), "aarch64-unknown-linux-gnu")
        .unwrap_err();

    assert!(matches!(
        error,
        TaskError::ToolMissing(message)
            if message.contains("aarch64-linux-gnu-gcc")
                && message.contains("zig")
    ));
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn cross_compiler_resolution_prefers_cross_gcc() {
    let (dir, _path) = fake_tool("aarch64-linux-gnu-gcc");
    let mut env = scoped_env(&["PATH", "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS"]);
    prepend_path(&mut env, dir.path());
    set_str(&mut env, "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "zig");

    let override_value =
        resolve_cross_compiler_for_tests(workspace_root(), "aarch64-unknown-linux-gnu")
            .expect("resolution succeeds")
            .expect("cross compiler override present");

    assert_eq!(
        override_value.0,
        OsString::from("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER")
    );
    assert_eq!(override_value.1, OsString::from("aarch64-linux-gnu-gcc"));
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn cross_compiler_resolution_falls_back_to_zig() {
    let (dir, _path) = fake_tool("zig");
    let mut env = scoped_env(&["PATH", "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS"]);
    prepend_path(&mut env, dir.path());
    set_str(
        &mut env,
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "aarch64-linux-gnu-gcc",
    );

    let override_value =
        resolve_cross_compiler_for_tests(workspace_root(), "aarch64-unknown-linux-gnu")
            .expect("resolution succeeds")
            .expect("cross compiler override present");

    assert_eq!(
        override_value.0,
        OsString::from("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER")
    );

    let override_path = PathBuf::from(&override_value.1);
    assert!(override_path.ends_with(zig_shim_name("aarch64-unknown-linux-gnu")));
    assert!(override_path.exists());
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn tarball_resolution_skips_targets_without_cross_tooling() {
    let mut env = scoped_env(&["OC_RSYNC_FORCE_MISSING_CARGO_TOOLS"]);
    set_str(
        &mut env,
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "aarch64-linux-gnu-gcc,zig",
    );

    let specs = vec![
        TarballSpec {
            platform: TarballPlatform::Linux,
            arch: "amd64",
            metadata_arch: "x86_64",
            target_triple: "x86_64-unknown-linux-gnu",
        },
        TarballSpec {
            platform: TarballPlatform::Linux,
            arch: "aarch64",
            metadata_arch: "aarch64",
            target_triple: "aarch64-unknown-linux-gnu",
        },
    ];

    let resolved = resolve_tarball_cross_compilers_for_tests(workspace_root(), specs)
        .expect("resolution succeeds");

    assert_eq!(resolved.builds.len(), 1);
    assert_eq!(
        resolved.builds[0].spec.target_triple,
        "x86_64-unknown-linux-gnu"
    );
    assert!(resolved.builds[0].linker.is_none());

    assert_eq!(resolved.skipped.len(), 1);
    assert_eq!(
        resolved.skipped[0].spec.target_triple,
        "aarch64-unknown-linux-gnu"
    );
    assert!(
        resolved.skipped[0]
            .message
            .contains("aarch64-linux-gnu-gcc")
    );
}

fn fake_rpmbuild_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp directory for fake rpmbuild");
    let rpmbuild_name = if cfg!(windows) {
        "rpmbuild.cmd"
    } else {
        "rpmbuild"
    };
    let rpmbuild_path = dir.path().join(rpmbuild_name);

    #[cfg(windows)]
    let script = b"@echo off\r\nexit /b 0\r\n".to_vec();

    #[cfg(not(windows))]
    let script = b"#!/bin/sh\nexit 0\n".to_vec();

    std::fs::write(&rpmbuild_path, script).expect("write fake rpmbuild script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&rpmbuild_path)
            .expect("read fake rpmbuild metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&rpmbuild_path, permissions)
            .expect("make fake rpmbuild executable");
    }

    (dir, rpmbuild_path)
}

#[cfg(all(test, target_os = "linux"))]
fn fake_tool(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp directory for fake tool");
    let file_name = if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_owned()
    };
    let tool_path = dir.path().join(file_name);

    #[cfg(windows)]
    let script = b"@echo off\r\nexit /b 0\r\n".to_vec();

    #[cfg(not(windows))]
    let script = b"#!/bin/sh\nexit 0\n".to_vec();

    std::fs::write(&tool_path, script).expect("write fake tool script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&tool_path)
            .expect("read fake tool metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&tool_path, permissions).expect("make fake tool executable");
    }

    (dir, tool_path)
}

#[cfg(all(test, target_os = "linux"))]
fn prepend_path(env: &mut ScopedEnv, directory: &Path) {
    let mut path_entries = vec![directory.to_path_buf()];
    if let Some(existing) = env::var_os("PATH") {
        path_entries.extend(env::split_paths(&existing));
    }
    let joined = env::join_paths(path_entries).expect("compose PATH");
    set_os(env, "PATH", joined.as_os_str());
}

#[cfg(all(test, target_os = "linux"))]
fn zig_shim_name(target: &str) -> String {
    if cfg!(windows) {
        format!("zig-linker-{target}.cmd")
    } else {
        format!("zig-linker-{target}")
    }
}
