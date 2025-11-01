use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, run_cargo_tool};
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header, HeaderMode};

/// Options accepted by the `package` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageOptions {
    /// Whether to build the Debian package.
    pub build_deb: bool,
    /// Whether to build the RPM package.
    pub build_rpm: bool,
    /// Whether to build the amd64 tarball distribution.
    pub build_tarball: bool,
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
    let mut build_tarball = false;
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

        if arg == "--tarball" {
            build_tarball = true;
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

    if !build_deb && !build_rpm && !build_tarball {
        build_deb = true;
        build_rpm = true;
        build_tarball = true;
    }

    Ok(PackageOptions {
        build_deb,
        build_rpm,
        build_tarball,
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
        build_workspace_binaries(workspace, &options.profile, None)?;
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

    if options.build_tarball {
        build_workspace_binaries(
            workspace,
            &options.profile,
            Some(branding.tarball_target.as_str()),
        )?;
        build_amd64_tarball(workspace, &branding, &options.profile)?;
    }

    Ok(())
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask package [OPTIONS]\n\nOptions:\n  --deb            Build only the Debian package\n  --rpm            Build only the RPM package\n  --tarball        Build only the amd64 tarball distribution\n  --release        Build using the release profile (default)\n  --debug          Build using the debug profile\n  --profile NAME   Build using the named cargo profile\n  --no-profile     Do not override the cargo profile\n  -h, --help       Show this help message",
    )
}

fn build_workspace_binaries(
    workspace: &Path,
    profile: &Option<OsString>,
    target: Option<&str>,
) -> TaskResult<()> {
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

    if let Some(target) = target {
        args.push(OsString::from("--target"));
        args.push(OsString::from(target));
    }

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

fn build_amd64_tarball(
    workspace: &Path,
    branding: &WorkspaceBranding,
    profile: &Option<OsString>,
) -> TaskResult<()> {
    let profile_name = tarball_profile_name(profile);
    let target_dir = workspace
        .join("target")
        .join(&branding.tarball_target)
        .join(&profile_name);

    let binaries = [
        (branding.client_bin.as_str(), 0o755),
        (branding.daemon_bin.as_str(), 0o755),
        (branding.legacy_client_bin.as_str(), 0o755),
        (branding.legacy_daemon_bin.as_str(), 0o755),
    ];

    for (name, _) in &binaries {
        let path = target_dir.join(name);
        ensure_tarball_source(&path)?;
    }

    let dist_dir = workspace.join("target").join("dist");
    fs::create_dir_all(&dist_dir)?;

    let root_name = format!(
        "{}-{}-{}",
        branding.client_bin, branding.rust_version, branding.tarball_target
    );
    let tarball_path = dist_dir.join(format!("{root_name}.tar.gz"));
    println!(
        "Building amd64 tarball distribution at {}",
        tarball_path.display()
    );

    let tarball_file = File::create(&tarball_path).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!(
                "failed to create tarball at {}: {error}",
                tarball_path.display()
            ),
        ))
    })?;
    let encoder = GzEncoder::new(tarball_file, Compression::default());
    let mut builder = Builder::new(encoder);
    builder.mode(HeaderMode::Deterministic);

    let directories = [
        root_name.clone(),
        format!("{root_name}/bin"),
        format!("{root_name}/libexec"),
        format!("{root_name}/libexec/oc-rsync"),
        format!("{root_name}/lib"),
        format!("{root_name}/lib/systemd"),
        format!("{root_name}/lib/systemd/system"),
        format!("{root_name}/etc"),
        format!("{root_name}/etc/oc-rsyncd"),
        format!("{root_name}/etc/default"),
    ];

    for directory in &directories {
        append_directory_entry(&mut builder, directory, 0o755)?;
    }

    let packaging_entries: Vec<(PathBuf, &str, u32)> = vec![
        (
            workspace.join("packaging/systemd/oc-rsyncd.service"),
            "lib/systemd/system/oc-rsyncd.service",
            0o644,
        ),
        (
            workspace.join("packaging/etc/oc-rsyncd/oc-rsyncd.conf"),
            "etc/oc-rsyncd/oc-rsyncd.conf",
            0o644,
        ),
        (
            workspace.join("packaging/etc/oc-rsyncd/oc-rsyncd.secrets"),
            "etc/oc-rsyncd/oc-rsyncd.secrets",
            0o600,
        ),
        (
            workspace.join("packaging/default/oc-rsyncd"),
            "etc/default/oc-rsyncd",
            0o644,
        ),
        (workspace.join("LICENSE"), "LICENSE", 0o644),
        (workspace.join("README.md"), "README.md", 0o644),
    ];

    for (source, _, _) in &packaging_entries {
        ensure_tarball_source(source)?;
    }

    let mut append_binary = |name: &str, relative: &str, mode: u32| -> TaskResult<()> {
        let source = target_dir.join(name);
        let destination = format!("{root_name}/{relative}");
        append_file_entry(&mut builder, &destination, &source, mode)
    };

    append_binary(
        branding.client_bin.as_str(),
        &format!("bin/{}", branding.client_bin),
        0o755,
    )?;
    append_binary(
        branding.daemon_bin.as_str(),
        &format!("bin/{}", branding.daemon_bin),
        0o755,
    )?;
    append_binary(
        branding.legacy_client_bin.as_str(),
        &format!("libexec/oc-rsync/{}", branding.legacy_client_bin),
        0o755,
    )?;
    append_binary(
        branding.legacy_daemon_bin.as_str(),
        &format!("libexec/oc-rsync/{}", branding.legacy_daemon_bin),
        0o755,
    )?;

    for (source, relative, mode) in &packaging_entries {
        let destination = format!("{root_name}/{relative}");
        append_file_entry(&mut builder, &destination, source, *mode)?;
    }

    let encoder = builder.into_inner()?;
    encoder.finish()?;

    Ok(())
}

fn tarball_profile_name(profile: &Option<OsString>) -> String {
    profile
        .as_ref()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("debug"))
}

fn ensure_tarball_source(path: &Path) -> TaskResult<()> {
    if path.is_file() {
        return Ok(());
    }

    Err(TaskError::Validation(format!(
        "tarball source file missing: {}",
        path.display()
    )))
}

fn append_directory_entry<W: Write>(
    builder: &mut Builder<W>,
    path: &str,
    mode: u32,
) -> TaskResult<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(0);
    header.set_path(path)?;
    header.set_cksum();
    builder.append(&header, io::empty())?;
    Ok(())
}

fn append_file_entry<W: Write>(
    builder: &mut Builder<W>,
    destination: &str,
    source: &Path,
    mode: u32,
) -> TaskResult<()> {
    let metadata = fs::metadata(source).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to read metadata for {}: {error}", source.display()),
        ))
    })?;

    if !metadata.is_file() {
        return Err(TaskError::Validation(format!(
            "expected regular file for tarball entry: {}",
            source.display()
        )));
    }

    let mut file = File::open(source).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to open {}: {error}", source.display()),
        ))
    })?;

    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(metadata.len());
    header.set_path(destination)?;
    header.set_cksum();
    builder.append(&header, &mut file)?;

    Ok(())
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
                build_tarball: true,
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

    #[test]
    fn parse_args_supports_tarball_only() {
        let options = parse_args([OsString::from("--tarball")]).expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: false,
                build_rpm: false,
                build_tarball: true,
                profile: Some(OsString::from("release")),
            }
        );
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
                build_tarball: false,
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
