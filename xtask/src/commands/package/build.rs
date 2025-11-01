use super::{DIST_PROFILE, PackageOptions, tarball};
use crate::error::TaskResult;
use crate::util::{
    ensure_command_available, probe_cargo_tool, run_cargo_tool, run_cargo_tool_with_env,
};
use crate::workspace::load_workspace_branding;
use std::env;
use std::ffi::OsString;
use std::path::Path;

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
        let specs = tarball::linux_tarball_specs(&branding)?;
        for spec in &specs {
            ensure_cross_compiler_available(spec.target_triple)?;
        }
        for spec in &specs {
            build_workspace_binaries(workspace, &options.profile, Some(spec.target_triple))?;
            tarball::build_tarball(workspace, &branding, &options.profile, spec)?;
        }
    }

    Ok(())
}

pub(super) fn build_workspace_binaries(
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

    let mut env_overrides: Vec<(OsString, OsString)> = Vec::new();

    if let Some(target) = target {
        if let Some(spec) = cross_compiler_for_target(target) {
            ensure_command_available(spec.program, spec.install_hint)?;
            env_overrides.push((linker_env_var_name(target), OsString::from(spec.program)));
        }
        args.push(OsString::from("--target"));
        args.push(OsString::from(target));
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

fn ensure_cross_compiler_available(target: &str) -> TaskResult<()> {
    if let Some(spec) = cross_compiler_for_target(target) {
        ensure_command_available(spec.program, spec.install_hint)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct CrossCompilerSpec {
    program: &'static str,
    install_hint: &'static str,
}

fn cross_compiler_for_target(target: &str) -> Option<CrossCompilerSpec> {
    match target {
        "aarch64-unknown-linux-gnu" => Some(CrossCompilerSpec {
            program: "aarch64-linux-gnu-gcc",
            install_hint: "install the aarch64-linux-gnu-gcc cross-compiler (for example, `apt install gcc-aarch64-linux-gnu`)",
        }),
        _ => None,
    }
}

fn linker_env_var_name(target: &str) -> OsString {
    let mut normalized = target.replace('-', "_");
    normalized.make_ascii_uppercase();
    OsString::from(format!("CARGO_TARGET_{}_LINKER", normalized))
}

#[cfg(test)]
pub(super) fn cross_compiler_program_for_target(target: &str) -> Option<&'static str> {
    cross_compiler_for_target(target).map(|spec| spec.program)
}
