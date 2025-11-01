use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, run_cargo_tool};
use crate::workspace::load_workspace_branding;
use std::env;
use std::ffi::OsString;
use std::path::Path;

/// Options accepted by the `package` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageOptions {
    /// Whether to build the Debian package.
    pub build_deb: bool,
    /// Whether to build the RPM package.
    pub build_rpm: bool,
    /// Optional profile override.
    pub profile: Option<OsString>,
}

/// Parses CLI arguments for the `package` command.
pub fn parse_args<I>(args: I) -> TaskResult<PackageOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut build_deb = false;
    let mut build_rpm = false;
    let mut profile = Some(OsString::from("release"));
    let mut profile_explicit = false;

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        if arg == "--deb" {
            build_deb = true;
            continue;
        }

        if arg == "--rpm" {
            build_rpm = true;
            continue;
        }

        if arg == "--release" {
            set_profile_option(
                &mut profile,
                &mut profile_explicit,
                Some(OsString::from("release")),
            )?;
            continue;
        }

        if arg == "--debug" {
            set_profile_option(
                &mut profile,
                &mut profile_explicit,
                Some(OsString::from("debug")),
            )?;
            continue;
        }

        if arg == "--no-profile" {
            set_profile_option(&mut profile, &mut profile_explicit, None)?;
            continue;
        }

        if arg == "--profile" {
            let value = args.next().ok_or_else(|| {
                TaskError::Usage(String::from(
                    "--profile requires a value; see `cargo xtask package --help`",
                ))
            })?;

            if value.is_empty() {
                return Err(TaskError::Usage(String::from(
                    "--profile requires a non-empty value",
                )));
            }

            set_profile_option(&mut profile, &mut profile_explicit, Some(value))?;
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for package command",
            arg.to_string_lossy()
        )));
    }

    if !build_deb && !build_rpm {
        build_deb = true;
        build_rpm = true;
    }

    Ok(PackageOptions {
        build_deb,
        build_rpm,
        profile,
    })
}

fn set_profile_option(
    profile: &mut Option<OsString>,
    explicit: &mut bool,
    value: Option<OsString>,
) -> TaskResult<()> {
    if *explicit {
        return Err(TaskError::Usage(String::from(
            "profile specified multiple times; choose at most one of --profile/--release/--debug/--no-profile",
        )));
    }

    *profile = value;
    *explicit = true;
    Ok(())
}

/// Executes the `package` command.
pub fn execute(workspace: &Path, options: PackageOptions) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    println!("Preparing {}", branding.summary());

    if options.build_deb || options.build_rpm {
        build_workspace_binaries(workspace, &options.profile)?;
    }

    if options.build_deb {
        println!("Building Debian package with cargo deb");
        let mut deb_args = vec![OsString::from("deb"), OsString::from("--locked")];
        if let Some(profile) = &options.profile {
            deb_args.push(OsString::from("--profile"));
            deb_args.push(profile.clone());
        }
        run_cargo_tool(
            workspace,
            deb_args,
            "cargo deb",
            "install the cargo-deb subcommand (cargo install cargo-deb)",
        )?;
    }

    if options.build_rpm {
        println!("Building RPM package with cargo rpm build");
        let mut rpm_args = vec![OsString::from("rpm"), OsString::from("build")];
        if let Some(profile) = &options.profile {
            rpm_args.push(OsString::from("--profile"));
            rpm_args.push(profile.clone());
        }
        run_cargo_tool(
            workspace,
            rpm_args,
            "cargo rpm build",
            "install the cargo-rpm subcommand (cargo install cargo-rpm)",
        )?;
    }

    Ok(())
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask package [OPTIONS]\n\nOptions:\n  --deb            Build only the Debian package\n  --rpm            Build only the RPM package\n  --release        Build using the release profile (default)\n  --debug          Build using the debug profile\n  --profile NAME   Build using the named cargo profile\n  --no-profile     Do not override the cargo profile\n  -h, --help       Show this help message",
    )
}

fn build_workspace_binaries(workspace: &Path, profile: &Option<OsString>) -> TaskResult<()> {
    if env::var_os("OC_RSYNC_PACKAGE_SKIP_BUILD").is_some() {
        println!("Skipping workspace binary build because OC_RSYNC_PACKAGE_SKIP_BUILD is set");
        return Ok(());
    }

    println!("Ensuring workspace binaries are built with cargo build");
    let mut args = vec![
        OsString::from("build"),
        OsString::from("--workspace"),
        OsString::from("--bins"),
        OsString::from("--locked"),
    ];

    args.push(OsString::from("--features"));
    args.push(OsString::from("legacy-binaries"));

    if let Some(profile) = profile {
        args.push(OsString::from("--profile"));
        args.push(profile.clone());
    }

    run_cargo_tool(
        workspace,
        args,
        "cargo build",
        "use `cargo build` to compile the workspace binaries",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: true,
                build_rpm: true,
                profile: Some(OsString::from("release")),
            }
        );
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_duplicate_profile_flags() {
        let error = parse_args([
            OsString::from("--profile"),
            OsString::from("release"),
            OsString::from("--debug"),
        ])
        .unwrap_err();
        assert!(matches!(
            error,
            TaskError::Usage(message) if message.contains("profile specified multiple times")
        ));
    }

    #[test]
    fn parse_args_requires_profile_value() {
        let error = parse_args([OsString::from("--profile")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--profile")));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("package")));
    }

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
                if let Some(previous) = previous {
                    unsafe { env::set_var(key, previous) };
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
                profile: Some(OsString::from("release")),
            },
        )
        .unwrap_err();
        assert!(matches!(error, TaskError::ToolMissing(message) if message.contains("cargo deb")));
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
                profile: Some(OsString::from("debug")),
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, TaskError::ToolMissing(message) if message.contains("cargo rpm build"))
        );
    }

    fn fake_cargo_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create temp directory for fake cargo");
        let cargo_name = if cfg!(windows) { "cargo.cmd" } else { "cargo" };
        let cargo_path = dir.path().join(cargo_name);

        #[cfg(windows)]
        let script =
            b"@echo off\r\necho error: no such subcommand: %1 1>&2\r\nexit /b 101\r\n".to_vec();

        #[cfg(not(windows))]
        let script =
            b"#!/bin/sh\necho 'error: no such subcommand: '\"$1\" >&2\nexit 101\n".to_vec();

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
}
