use super::{DIST_PROFILE, PackageOptions, tarball};
use crate::error::{TaskError, TaskResult};
use crate::util::{
    ensure_command_available, ensure_rust_target_installed, probe_cargo_tool, run_cargo_tool,
    run_cargo_tool_with_env,
};
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Executes the `package` command.
pub fn execute(workspace: &Path, options: PackageOptions) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    println!("Preparing {}", branding.summary());

    if options.build_deb || options.build_rpm {
        build_workspace_binaries(workspace, &branding, &options.profile, None, None, true)?;
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
        let profile_label = options
            .profile
            .as_deref()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("debug"));

        let msg = format!("Building RPM package with cargo rpm build (profile={profile_label})");
        println!("{msg}");

        ensure_command_available(
            "rpmbuild",
            "install the rpmbuild tooling (for example, `dnf install rpm-build` or `apt install rpm`)",
        )?;

        if let Some(profile) = options.profile.as_deref() {
            if profile != "release" && profile != DIST_PROFILE {
                probe_cargo_tool(
                    workspace,
                    &["rpm", "--help"],
                    "cargo rpm build",
                    "install the cargo-rpm subcommand (cargo install cargo-rpm)",
                )?;
                return Err(crate::util::validation_error(format!(
                    "cargo rpm build does not support overriding the cargo profile (requested '{}'); adjust [package.metadata.rpm.cargo].buildflags instead",
                    profile.to_string_lossy()
                )));
            }
        }

        let rpm_output_rel = std::path::Path::new("target").join("dist");
        let rpm_output_abs = workspace.join(&rpm_output_rel);
        fs::create_dir_all(&rpm_output_abs)?;

        let rpm_args = vec![
            OsString::from("rpm"),
            OsString::from("build"),
            OsString::from("--output"),
            OsString::from(rpm_output_rel.to_string_lossy().into_owned()),
        ];

        run_cargo_tool(
            workspace,
            rpm_args,
            "cargo rpm build",
            "install the cargo-rpm subcommand (cargo install cargo-rpm)",
        )?;
    }

    if options.build_tarball {
        let specs = tarball::tarball_specs(&branding, options.tarball_target.as_deref())?;
        let resolved = resolve_tarball_builds(workspace, specs)?;

        if resolved.builds.is_empty() {
            if let Some(skipped) = resolved.skipped.first() {
                return Err(TaskError::ToolMissing(skipped.message.clone()));
            }

            return Err(TaskError::ToolMissing(String::from(
                "no tarball targets available for packaging",
            )));
        }

        for skipped in &resolved.skipped {
            println!(
                "Skipping {target} tar.gz distribution because cross-compilation tooling is unavailable: {message}",
                target = skipped.spec.display_name(),
                message = skipped.message
            );
        }

        for build in &resolved.builds {
            build_workspace_binaries(
                workspace,
                &branding,
                &options.profile,
                Some(build.spec.target_triple),
                build.linker.as_ref(),
                true,
            )?;
            tarball::build_tarball(workspace, &branding, &options.profile, &build.spec)?;
        }
    }

    Ok(())
}

fn resolve_tarball_builds(
    workspace: &Path,
    specs: Vec<tarball::TarballSpec>,
) -> TaskResult<ResolvedTarballSpecs> {
    let mut builds = Vec::with_capacity(specs.len());
    let mut skipped = Vec::new();

    for spec in specs {
        match resolve_cross_compiler(workspace, &spec) {
            Ok(linker) => builds.push(TarballBuild { spec, linker }),
            Err(TaskError::ToolMissing(message)) => {
                skipped.push(SkippedTarballSpec { spec, message })
            }
            Err(error) => return Err(error),
        }
    }

    Ok(ResolvedTarballSpecs { builds, skipped })
}

#[cfg(test)]
pub(super) fn resolve_tarball_cross_compilers_for_tests(
    workspace: &Path,
    specs: Vec<tarball::TarballSpec>,
) -> TaskResult<ResolvedTarballSpecs> {
    resolve_tarball_builds(workspace, specs)
}

pub(super) fn build_workspace_binaries(
    workspace: &Path,
    branding: &WorkspaceBranding,
    profile: &Option<OsString>,
    target: Option<&str>,
    linker_override: Option<&LinkerOverride>,
    include_legacy: bool,
) -> TaskResult<()> {
    if let Some(target) = target {
        ensure_rust_target_installed(target)?;
    }

    if env::var_os("OC_RSYNC_PACKAGE_SKIP_BUILD").is_some() {
        println!("Skipping workspace binary build because OC_RSYNC_PACKAGE_SKIP_BUILD is set");
        return Ok(());
    }

    println!("Ensuring workspace binaries are built with cargo build");
    let command = workspace_build_command(profile, target, linker_override)?;

    run_cargo_tool_with_env(
        workspace,
        command.args,
        &command.env_overrides,
        "cargo build",
        "use `cargo build` to compile the workspace binaries",
    )?;

    if include_legacy {
        ensure_legacy_launchers(workspace, branding, profile, target)?;
    }

    Ok(())
}

fn linker_env_var_name(target: &str) -> OsString {
    let mut normalized = target.replace('-', "_");
    normalized.make_ascii_uppercase();
    OsString::from(format!("CARGO_TARGET_{normalized}_LINKER"))
}

/// Command object describing how to invoke `cargo build` for the workspace.
#[derive(Clone, Debug)]
pub(super) struct WorkspaceBuildCommand {
    pub args: Vec<OsString>,
    pub env_overrides: EnvOverrides,
}

/// Environment override list for the build command.
pub(super) type EnvOverrides = Vec<(OsString, OsString)>;

#[derive(Clone, Debug)]
pub(super) struct LinkerOverride {
    env_var: OsString,
    value: OsString,
}

#[cfg_attr(test, derive(Debug))]
pub(super) struct ResolvedTarballSpecs {
    pub builds: Vec<TarballBuild>,
    pub skipped: Vec<SkippedTarballSpec>,
}

#[cfg_attr(test, derive(Debug))]
pub(super) struct TarballBuild {
    pub spec: tarball::TarballSpec,
    pub linker: Option<LinkerOverride>,
}

#[cfg_attr(test, derive(Debug))]
pub(super) struct SkippedTarballSpec {
    pub spec: tarball::TarballSpec,
    pub message: String,
}

fn resolve_cross_compiler(
    workspace: &Path,
    spec: &tarball::TarballSpec,
) -> TaskResult<Option<LinkerOverride>> {
    if !spec.requires_cross_compiler() {
        return Ok(None);
    }

    let Some(candidates) = cross_compiler_candidates(spec.target_triple) else {
        return Ok(None);
    };

    for candidate in &candidates {
        match ensure_command_available(candidate.program(), candidate.install_hint) {
            Ok(()) => {
                return candidate.configure(workspace, spec.target_triple);
            }
            Err(TaskError::ToolMissing(_)) => {
                continue;
            }
            Err(error) => {
                return Err(error);
            }
        }
    }

    Err(missing_cross_compiler_error(
        spec.target_triple,
        &candidates,
    ))
}

/// Builder pattern for constructing `WorkspaceBuildCommand` instances.
///
/// This keeps argument construction composable and explicit, while satisfying
/// clippy's type-complexity constraints.
#[derive(Debug)]
struct WorkspaceBuildCommandBuilder {
    args: Vec<OsString>,
    env_overrides: EnvOverrides,
}

impl WorkspaceBuildCommandBuilder {
    fn new() -> Self {
        Self {
            args: vec![
                OsString::from("build"),
                OsString::from("--workspace"),
                OsString::from("--bins"),
                OsString::from("--locked"),
            ],
            env_overrides: Vec::new(),
        }
    }

    fn with_target(mut self, target: &str, linker_override: Option<&LinkerOverride>) -> Self {
        self.args.push(OsString::from("--target"));
        self.args.push(OsString::from(target));
        if let Some(linker) = linker_override {
            self.env_overrides
                .push((linker.env_var.clone(), linker.value.clone()));
        }
        self
    }

    fn with_profile(mut self, profile: &OsString) -> Self {
        self.args.push(OsString::from("--profile"));
        self.args.push(profile.clone());
        self
    }

    fn with_features_from_env(mut self) -> TaskResult<Self> {
        configure_feature_args(&mut self.args)?;
        Ok(self)
    }

    fn build(self) -> WorkspaceBuildCommand {
        WorkspaceBuildCommand {
            args: self.args,
            env_overrides: self.env_overrides,
        }
    }
}

fn workspace_build_command(
    profile: &Option<OsString>,
    target: Option<&str>,
    linker_override: Option<&LinkerOverride>,
) -> TaskResult<WorkspaceBuildCommand> {
    let mut builder = WorkspaceBuildCommandBuilder::new();

    if let Some(target) = target {
        builder = builder.with_target(target, linker_override);
    }

    if let Some(profile) = profile {
        builder = builder.with_profile(profile);
    }

    let builder = builder.with_features_from_env()?;
    Ok(builder.build())
}

fn configure_feature_args(args: &mut Vec<OsString>) -> TaskResult<()> {
    const DEFAULT_FEATURE_ENV: &str = "OC_RSYNC_PACKAGE_DEFAULT_FEATURES";
    const FEATURE_LIST_ENV: &str = "OC_RSYNC_PACKAGE_FEATURES";

    let default_features = match env::var_os(DEFAULT_FEATURE_ENV) {
        Some(value) => parse_env_bool(&value, DEFAULT_FEATURE_ENV)?,
        None => true,
    };

    if !default_features {
        args.push(OsString::from("--no-default-features"));
    }

    if let Some(features) = env::var_os(FEATURE_LIST_ENV) {
        if !features.is_empty() {
            let features_value = features_to_string(&features, FEATURE_LIST_ENV)?;
            if !features_value.is_empty() {
                args.push(OsString::from("--features"));
                args.push(OsString::from(features_value));
            }
        }
    }

    Ok(())
}

fn parse_env_bool(value: &OsStr, var: &str) -> TaskResult<bool> {
    let text = value.to_string_lossy();
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(TaskError::Validation(format!(
            "environment variable {var} must be a boolean value (1/0/true/false) but was '{other}'"
        ))),
    }
}

fn features_to_string(value: &OsStr, var: &str) -> TaskResult<String> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    if trimmed.contains(['\n', '\r']) {
        return Err(TaskError::Validation(format!(
            "environment variable {var} must not contain newlines"
        )));
    }

    Ok(trimmed.to_string())
}

fn ensure_legacy_launchers(
    workspace: &Path,
    branding: &WorkspaceBranding,
    profile: &Option<OsString>,
    target: Option<&str>,
) -> TaskResult<()> {
    let mut base = workspace.join("target");
    if let Some(target) = target {
        base.push(target);
    }

    let profile_dir = profile
        .as_ref()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("debug"));
    base.push(&profile_dir);

    let extension = if target.map_or(cfg!(windows), |triple| triple.contains("windows")) {
        ".exe"
    } else {
        ""
    };

    let canonical_client = base.join(format!("{}{}", branding.client_bin, extension));
    let legacy_client = base.join(format!("{}{}", branding.legacy_client_bin, extension));
    if canonical_client.is_file() && !legacy_client.is_file() {
        fs::copy(&canonical_client, &legacy_client).map_err(|error| {
            TaskError::Io(std::io::Error::new(
                error.kind(),
                format!(
                    "failed to create legacy client launcher at {}: {error}",
                    legacy_client.display()
                ),
            ))
        })?;
    }

    let canonical_daemon = base.join(format!("{}{}", branding.daemon_bin, extension));
    let legacy_daemon = base.join(format!("{}{}", branding.legacy_daemon_bin, extension));
    if canonical_daemon.is_file() && !legacy_daemon.is_file() {
        fs::copy(&canonical_daemon, &legacy_daemon).map_err(|error| {
            TaskError::Io(std::io::Error::new(
                error.kind(),
                format!(
                    "failed to create legacy daemon launcher at {}: {error}",
                    legacy_daemon.display()
                ),
            ))
        })?;
    }

    Ok(())
}

fn missing_cross_compiler_error(target: &str, candidates: &[CrossCompilerCandidate]) -> TaskError {
    if candidates.len() == 1 {
        let candidate = &candidates[0];
        return TaskError::ToolMissing(format!(
            "{} is unavailable; {}",
            candidate.display_name, candidate.install_hint
        ));
    }

    let mut options = String::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if index > 0 {
            if index + 1 == candidates.len() {
                options.push_str(" or ");
            } else {
                options.push_str(", ");
            }
        }
        options.push_str(candidate.display_name);
        options.push_str(" (");
        options.push_str(candidate.install_hint);
        options.push(')');
    }

    TaskError::ToolMissing(format!(
        "unable to locate cross-compilation tooling for target {target}; install {options}"
    ))
}

#[derive(Clone, Copy)]
struct CrossCompilerCandidate {
    display_name: &'static str,
    install_hint: &'static str,
    strategy: CrossCompilerStrategy,
}

impl CrossCompilerCandidate {
    fn program(&self) -> &'static str {
        match self.strategy {
            CrossCompilerStrategy::Direct { program } => program,
            CrossCompilerStrategy::Zig { program, .. } => program,
        }
    }

    fn configure(&self, workspace: &Path, target: &str) -> TaskResult<Option<LinkerOverride>> {
        let env_var = linker_env_var_name(target);
        let value = match self.strategy {
            CrossCompilerStrategy::Direct { program } => OsString::from(program),
            CrossCompilerStrategy::Zig {
                program: _,
                zig_target,
            } => create_zig_linker_shim(workspace, target, zig_target)?,
        };

        Ok(Some(LinkerOverride { env_var, value }))
    }
}

#[derive(Clone, Copy)]
enum CrossCompilerStrategy {
    Direct {
        program: &'static str,
    },
    Zig {
        program: &'static str,
        zig_target: &'static str,
    },
}

fn cross_compiler_candidates(target: &str) -> Option<Vec<CrossCompilerCandidate>> {
    match target {
        "aarch64-unknown-linux-gnu" => Some(vec![
            CrossCompilerCandidate {
                display_name: "aarch64-linux-gnu-gcc",
                install_hint: "install the aarch64-linux-gnu-gcc cross-compiler (for example, `apt install gcc-aarch64-linux-gnu`)",
                strategy: CrossCompilerStrategy::Direct {
                    program: "aarch64-linux-gnu-gcc",
                },
            },
            CrossCompilerCandidate {
                display_name: "zig cc",
                install_hint: "install the zig compiler (for example, `apt install zig` or `brew install zig`)",
                strategy: CrossCompilerStrategy::Zig {
                    program: "zig",
                    zig_target: "aarch64-linux-gnu",
                },
            },
        ]),
        _ => None,
    }
}

fn create_zig_linker_shim(
    workspace: &Path,
    target: &str,
    zig_target: &str,
) -> TaskResult<OsString> {
    let shim_dir = workspace.join("target").join("xtask");
    fs::create_dir_all(&shim_dir)?;

    #[cfg(windows)]
    let shim_name = format!("zig-linker-{target}.cmd");
    #[cfg(not(windows))]
    let shim_name = format!("zig-linker-{target}");
    let shim_path = shim_dir.join(&shim_name);

    let mut file = File::create(&shim_path)?;
    #[cfg(windows)]
    {
        writeln!(file, "@echo off")?;
        writeln!(file, "zig cc -target {zig_target} %*")?;
    }
    #[cfg(not(windows))]
    {
        writeln!(file, "#!/bin/sh")?;
        writeln!(file, "exec zig cc -target {zig_target} \"$@\"")?;
    }
    drop(file);

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&shim_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&shim_path, permissions)?;
    }

    Ok(shim_path.into_os_string())
}

#[cfg(test)]
#[allow(unsafe_code)]
pub(super) fn resolve_cross_compiler_for_tests(
    workspace: &Path,
    target: &str,
) -> TaskResult<Option<(OsString, OsString)>> {
    let spec = match target {
        "x86_64-unknown-linux-gnu" => tarball::TarballSpec {
            platform: tarball::TarballPlatform::Linux,
            arch: "amd64",
            metadata_arch: "x86_64",
            target_triple: "x86_64-unknown-linux-gnu",
        },
        "aarch64-unknown-linux-gnu" => tarball::TarballSpec {
            platform: tarball::TarballPlatform::Linux,
            arch: "aarch64",
            metadata_arch: "aarch64",
            target_triple: "aarch64-unknown-linux-gnu",
        },
        other => {
            return Err(TaskError::Validation(format!(
                "unsupported test target '{other}'"
            )));
        }
    };

    resolve_cross_compiler(workspace, &spec).map(|override_opt| {
        override_opt.map(|override_value| (override_value.env_var, override_value.value))
    })
}

#[cfg(test)]
#[allow(unsafe_code)]
mod build_command_tests {
    use super::{
        configure_feature_args, ensure_legacy_launchers, parse_env_bool, workspace_build_command,
    };
    use crate::error::TaskError;
    use crate::workspace::load_workspace_branding;
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    struct EnvGuard {
        entries: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            let mut entries = Vec::with_capacity(keys.len());
            for key in keys {
                entries.push((*key, env::var_os(key)));
            }
            Self { entries }
        }

        fn set(&mut self, key: &'static str, value: &str) {
            if self.entries.iter().all(|(existing, _)| existing != &key) {
                self.entries.push((key, env::var_os(key)));
            }
            unsafe {
                env::set_var(key, value);
            }
        }

        fn unset(&mut self, key: &'static str) {
            if self.entries.iter().all(|(existing, _)| existing != &key) {
                self.entries.push((key, env::var_os(key)));
            }
            unsafe {
                env::remove_var(key);
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.entries.drain(..).rev() {
                if let Some(existing) = value {
                    unsafe {
                        env::set_var(key, existing);
                    }
                } else {
                    unsafe {
                        env::remove_var(key);
                    }
                }
            }
        }
    }

    #[test]
    fn parse_env_bool_accepts_truthy_values() {
        assert!(parse_env_bool(OsStr::new("true"), "VAR").unwrap());
        assert!(parse_env_bool(OsStr::new("1"), "VAR").unwrap());
        assert!(!parse_env_bool(OsStr::new("0"), "VAR").unwrap());
        assert!(!parse_env_bool(OsStr::new("off"), "VAR").unwrap());
    }

    #[test]
    fn parse_env_bool_rejects_invalid_values() {
        let err = parse_env_bool(OsStr::new("maybe"), "VAR").unwrap_err();
        assert!(matches!(err, TaskError::Validation(message) if message.contains("VAR")));
    }

    #[test]
    fn workspace_build_command_includes_feature_overrides() {
        let mut guard = EnvGuard::capture(&[
            "OC_RSYNC_PACKAGE_DEFAULT_FEATURES",
            "OC_RSYNC_PACKAGE_FEATURES",
        ]);
        guard.set("OC_RSYNC_PACKAGE_DEFAULT_FEATURES", "0");
        guard.set("OC_RSYNC_PACKAGE_FEATURES", "zstd lz4 iconv");

        let command = workspace_build_command(&Some(OsString::from("dist")), None, None)
            .expect("command builds");
        let args: Vec<String> = command
            .args
            .iter()
            .map(|value| value.to_string_lossy().into())
            .collect();
        assert!(args.contains(&"--no-default-features".to_string()));
        assert!(args.contains(&"--features".to_string()));
        assert!(args.contains(&"zstd lz4 iconv".to_string()));
    }

    #[test]
    fn configure_feature_args_ignores_empty_feature_sets() {
        let mut guard = EnvGuard::capture(&["OC_RSYNC_PACKAGE_FEATURES"]);
        guard.set("OC_RSYNC_PACKAGE_FEATURES", "   ");
        let mut args = Vec::new();
        configure_feature_args(&mut args).expect("configure succeeds");
        assert!(args.is_empty());
    }

    #[test]
    fn ensure_legacy_launchers_creates_copies() {
        let workspace = workspace_root();
        let branding = load_workspace_branding(workspace).expect("load branding");
        let mut guard = EnvGuard::capture(&["OC_RSYNC_PACKAGE_SKIP_BUILD"]);
        guard.unset("OC_RSYNC_PACKAGE_SKIP_BUILD");

        let extension = if cfg!(windows) { ".exe" } else { "" };
        let base = workspace.join("target").join("dist");
        std::fs::create_dir_all(&base).expect("create dist directory");
        let canonical = base.join(format!("{}{}", branding.client_bin, extension));
        std::fs::write(&canonical, b"binary").expect("create canonical binary placeholder");

        // Skip invoking cargo; instead rely on placeholders.
        unsafe {
            env::set_var("OC_RSYNC_PACKAGE_SKIP_BUILD", "1");
        }
        ensure_legacy_launchers(workspace, &branding, &Some(OsString::from("dist")), None)
            .expect("ensure legacy launchers succeeds");

        let legacy = base.join(format!("{}{}", branding.legacy_client_bin, extension));
        assert!(legacy.exists(), "legacy client launcher should exist");
        std::fs::remove_file(&legacy).ok();
        std::fs::remove_file(&canonical).ok();
    }

    fn workspace_root() -> &'static Path {
        static ROOT: std::sync::OnceLock<Box<Path>> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .into()
        })
    }
}
