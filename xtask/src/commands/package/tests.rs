use super::{PackageOptions, execute};
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

struct EnvGuards {
    previous: Vec<(&'static str, Option<OsString>)>,
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuards {
    #[allow(unsafe_code)]
    fn set_many(pairs: &[(&'static str, &str)]) -> Self {
        let lock = env_lock().lock().unwrap();
        let mut previous = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            previous.push((*key, env::var_os(key)));
            unsafe { env::set_var(key, value) };
        }
        drop(lock);
        Self { previous }
    }
}

impl Drop for EnvGuards {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        let _lock = env_lock().lock().unwrap();
        for (key, previous) in self.previous.drain(..).rev() {
            if let Some(value) = previous {
                unsafe { env::set_var(key, value) };
            } else {
                unsafe { env::remove_var(key) };
            }
        }
    }
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        Self::set_os(key, OsStr::new(value))
    }

    #[allow(unsafe_code)]
    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let lock = env_lock().lock().unwrap();
        let previous = env::var_os(key);
        unsafe { env::set_var(key, value) };
        drop(lock);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        let _lock = env_lock().lock().unwrap();
        if let Some(previous) = self.previous.take() {
            unsafe { env::set_var(self.key, previous) };
        } else {
            unsafe { env::remove_var(self.key) };
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
    let _env = EnvGuards::set_many(&[
        ("OC_RSYNC_PACKAGE_SKIP_BUILD", "1"),
        ("OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "cargo deb"),
    ]);
    let _env = EnvGuard::set("OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    let (_fake_cargo_dir, fake_cargo) = fake_cargo_path();
    let _cargo = EnvGuard::set_os("CARGO", fake_cargo.as_os_str());
    let error = execute(
        workspace_root(),
        PackageOptions {
            build_deb: true,
            build_rpm: false,
            build_tarball: false,
            profile: Some(OsString::from("release")),
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
    let _env = EnvGuards::set_many(&[
        ("OC_RSYNC_PACKAGE_SKIP_BUILD", "1"),
        ("OC_RSYNC_FORCE_MISSING_CARGO_TOOLS", "cargo rpm build"),
    ]);
    let _env = EnvGuard::set("OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
    let (_fake_cargo_dir, fake_cargo) = fake_cargo_path();
    let _cargo = EnvGuard::set_os("CARGO", fake_cargo.as_os_str());
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

fn fake_cargo_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp directory for fake cargo");
    let cargo_name = if cfg!(windows) { "cargo.cmd" } else { "cargo" };
    let cargo_path = dir.path().join(cargo_name);

    #[cfg(windows)]
    let script =
        b"@echo off\r\necho error: no such subcommand: %1 1>&2\r\nexit /b 101\r\n".to_vec();

    #[cfg(not(windows))]
    let script = b"#!/bin/sh\necho 'error: no such subcommand: '\"$1\" >&2\nexit 101\n".to_vec();

    std::fs::write(&cargo_path, script).expect("write fake cargo script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&cargo_path)
            .expect("read fake cargo metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&cargo_path, permissions).expect("make fake cargo executable");
    }

    (dir, cargo_path)
}
