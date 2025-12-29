#[cfg(test)]
use crate::error::{TaskError, TaskResult};
#[cfg(test)]
use crate::util::is_help_flag;
use std::ffi::OsString;

/// Optimised cargo profile used for release packaging.
pub const DIST_PROFILE: &str = "dist";

/// Options accepted by the `package` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageOptions {
    /// Whether to build the Debian package.
    pub build_deb: bool,
    /// Whether to build the RPM package.
    pub build_rpm: bool,
    /// Whether to build the Linux tarball distributions.
    pub build_tarball: bool,
    /// Optional target triple override for tarball packaging.
    pub tarball_target: Option<OsString>,
    /// Optional profile override.
    pub profile: Option<OsString>,
}

impl PackageOptions {
    /// Returns package options that build all release artifacts using the
    /// optimised distribution profile.
    pub fn release_all() -> Self {
        Self {
            build_deb: true,
            build_rpm: true,
            build_tarball: true,
            tarball_target: None,
            profile: Some(OsString::from(DIST_PROFILE)),
        }
    }
}

/// Parses CLI arguments for the `package` command.
#[cfg(test)]
pub fn parse_args<I>(args: I) -> TaskResult<PackageOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut build_deb = false;
    let mut build_rpm = false;
    let mut build_tarball = false;
    let mut profile = Some(OsString::from(DIST_PROFILE));
    let mut tarball_target = None;
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

        if arg == "--tarball-target" {
            let value = args.next().ok_or_else(|| {
                TaskError::Usage(String::from(
                    "--tarball-target requires a value; see `cargo xtask package --help`",
                ))
            })?;

            if value.is_empty() {
                return Err(TaskError::Usage(String::from(
                    "--tarball-target requires a non-empty value",
                )));
            }

            if tarball_target.is_some() {
                return Err(TaskError::Usage(String::from(
                    "--tarball-target specified multiple times",
                )));
            }

            tarball_target = Some(value);
            continue;
        }

        if arg == "--release" {
            set_profile_option(
                &mut profile,
                &mut profile_explicit,
                Some(OsString::from(DIST_PROFILE)),
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
        tarball_target,
        profile,
    })
}

#[cfg(test)]
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

/// Returns usage text for the command.
#[cfg(test)]
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask package [OPTIONS]\n\nOptions:\n  --deb            Build only the Debian package\n  --rpm            Build only the RPM package\n  --tarball        Build only the tar.gz distributions\n  --tarball-target TARGET\n                  Restrict tarball generation to the specified target triple\n  --release        Build using the dist profile (default)\n  --debug          Build using the debug profile\n  --profile NAME   Build using the named cargo profile\n  --no-profile     Do not override the cargo profile\n  -h, --help       Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, PackageOptions::release_all());
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
                tarball_target: None,
                profile: Some(OsString::from(DIST_PROFILE)),
            }
        );
    }

    #[test]
    fn parse_args_supports_tarball_target() {
        let options = parse_args([
            OsString::from("--tarball"),
            OsString::from("--tarball-target"),
            OsString::from("x86_64-unknown-linux-gnu"),
        ])
        .expect("parse succeeds");
        assert_eq!(
            options,
            PackageOptions {
                build_deb: false,
                build_rpm: false,
                build_tarball: true,
                tarball_target: Some(OsString::from("x86_64-unknown-linux-gnu")),
                profile: Some(OsString::from(DIST_PROFILE)),
            }
        );
    }

    #[test]
    fn parse_args_rejects_duplicate_tarball_target() {
        let error = parse_args([
            OsString::from("--tarball-target"),
            OsString::from("foo"),
            OsString::from("--tarball-target"),
            OsString::from("bar"),
        ])
        .unwrap_err();
        assert!(matches!(
            error,
            TaskError::Usage(message) if message.contains("--tarball-target specified multiple times")
        ));
    }

    #[test]
    fn parse_args_requires_tarball_target_value() {
        let error = parse_args([OsString::from("--tarball-target")]).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Usage(message) if message.contains("--tarball-target requires a value")
        ));
    }

    #[test]
    fn release_all_returns_dist_profile_with_all_targets() {
        let options = PackageOptions::release_all();
        assert!(options.build_deb);
        assert!(options.build_rpm);
        assert!(options.build_tarball);
        assert_eq!(options.profile, Some(OsString::from(DIST_PROFILE)));
    }
}
