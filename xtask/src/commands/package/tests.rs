#[cfg(test)]
use super::build::cross_compiler_program_for_target;
use super::{DIST_PROFILE, PackageOptions, execute};
use crate::error::TaskError;
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

fn env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

struct ScopedEnv {
    previous: Vec<(&'static str, Option<OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ScopedEnv {
    fn new(keys: &[&'static str]) -> Self {
        let lock = env_lock().lock().unwrap();
        let mut previous = Vec::with_capacity(keys.len());
        for key in keys {
            previous.push((*key, env::var_os(key)));
        }
        Self {
            previous,
            _lock: lock,
        }
    }

    fn ensure_tracked(&mut self, key: &'static str) {
        if self.previous.iter().any(|(existing, _)| existing == &key) {
            return;
        }

        self.previous.push((key, env::var_os(key)));
    }

    #[allow(unsafe_code)]
    fn set_os(&mut self, key: &'static str, value: &OsStr) {
        self.ensure_tracked(key);
        unsafe { env::set_var(key, value) };
    }

    fn set_str(&mut self, key: &'static str, value: &str) {
        self.set_os(key, OsStr::new(value));
    }
}

impl Drop for ScopedEnv {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        for (key, previous) in self.previous.drain(..).rev() {
            if let Some(value) = previous {
                unsafe { env::set_var(key, value) };
            } else {
                unsafe { env::remove_var(key) };
            }
        }
    }
}

#[test]
fn execute_with_no_targets_returns_success() {
    execute(
        workspace_root(),
        PackageOptions {
            build_deb: false,
            build_rpm: false,
            build_tarball: false,
            profile: None,
        },
    )
    .expect("execution succeeds when nothing to build");
}

#[test]
fn execute_reports_missing_cargo_deb_tool() {
    let mut env = ScopedEnv::new(&[
        "OC_RSYNC_PACKAGE_SKIP_BUILD",
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
    ]);
    env.set_str("OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    env.set_str("OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "cargo deb");
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: true,
            build_rpm: false,
            build_tarball: false,
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
    let mut env = ScopedEnv::new(&[
        "OC_RSYNC_PACKAGE_SKIP_BUILD",
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "PATH",
    ]);
    env.set_str("OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    env.set_str("OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "cargo rpm build");
    let (fake_rpmbuild_dir, _fake_rpmbuild) = fake_rpmbuild_path();
    let mut path_entries = vec![fake_rpmbuild_dir.path().to_path_buf()];
    if let Some(existing) = env::var_os("PATH") {
        path_entries.extend(env::split_paths(&existing));
    }
    let joined_path = env::join_paths(path_entries).expect("compose PATH with fake rpmbuild");
    env.set_os("PATH", joined_path.as_os_str());
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: false,
            build_rpm: true,
            build_tarball: false,
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
    let mut env = ScopedEnv::new(&[
        "OC_RSYNC_PACKAGE_SKIP_BUILD",
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
    ]);
    env.set_str("OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    env.set_str("OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "rpmbuild");
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: false,
            build_rpm: true,
            build_tarball: false,
            profile: Some(OsString::from(DIST_PROFILE)),
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TaskError::ToolMissing(message) if message.contains("rpmbuild")
    ));
}

#[test]
fn cross_compiler_detection_handles_known_targets() {
    assert_eq!(
        cross_compiler_program_for_target("aarch64-unknown-linux-gnu"),
        Some("aarch64-linux-gnu-gcc")
    );
    assert_eq!(
        cross_compiler_program_for_target("x86_64-unknown-linux-gnu"),
        None
    );
}

#[test]
fn execute_reports_missing_cross_compiler() {
    let mut env = ScopedEnv::new(&["OC_RSYNC_FORCE_MISSING_CARGO_TOOLS"]);
    env.set_str(
        "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS",
        "aarch64-linux-gnu-gcc",
    );

    let error = super::build::build_workspace_binaries(
        workspace_root(),
        &Some(OsString::from(DIST_PROFILE)),
        Some("aarch64-unknown-linux-gnu"),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        TaskError::ToolMissing(message) if message.contains("aarch64-linux-gnu-gcc")
    ));
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
