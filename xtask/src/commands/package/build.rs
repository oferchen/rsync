use super::{DIST_PROFILE, PackageOptions, tarball};
use crate::error::{TaskError, TaskResult};
use crate::util::{
    ensure_command_available, ensure_rust_target_installed, probe_cargo_tool, run_cargo_tool,
    run_cargo_tool_with_env,
};
use crate::workspace::load_workspace_branding;
use std::env;
use std::ffi::OsString;
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
        build_workspace_binaries(workspace, &options.profile, None, None)?;
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
        let rpm_args = vec![OsString::from("rpm"), OsString::from("build")];
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
                &options.profile,
                Some(build.spec.target_triple),
                build.linker.as_ref(),
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
    profile: &Option<OsString>,
    target: Option<&str>,
    linker_override: Option<&LinkerOverride>,
) -> TaskResult<()> {
    if let Some(target) = target {
        ensure_rust_target_installed(target)?;
    }

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

    let mut env_overrides: Vec<(OsString, OsString)> = Vec::new();

    if let Some(target) = target {
        args.push(OsString::from("--target"));
        args.push(OsString::from(target));
        if let Some(linker) = linker_override {
            env_overrides.push((linker.env_var.clone(), linker.value.clone()));
        }
    }

    if let Some(profile) = profile {
        args.push(OsString::from("--profile"));
        args.push(profile.clone());
    }

    run_cargo_tool_with_env(
        workspace,
        args,
        &env_overrides,
        "cargo build",
        "use `cargo build` to compile the workspace binaries",
    )
}

fn linker_env_var_name(target: &str) -> OsString {
    let mut normalized = target.replace('-', "_");
    normalized.make_ascii_uppercase();
    OsString::from(format!("CARGO_TARGET_{normalized}_LINKER"))
}

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
