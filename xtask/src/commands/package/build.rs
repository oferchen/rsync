use super::{AMD64_TARBALL_TARGET, PackageOptions, tarball};
use crate::error::TaskResult;
use crate::util::run_cargo_tool;
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
        build_workspace_binaries(workspace, &options.profile, Some(AMD64_TARBALL_TARGET))?;
        tarball::build_amd64_tarball(workspace, &branding, &options.profile)?;
    }

    Ok(())
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
