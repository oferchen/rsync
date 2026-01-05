use crate::cli::PackageArgs;
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
    /// Optional deb variant (e.g., "focal" for Ubuntu 20.04).
    pub deb_variant: Option<String>,
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
            deb_variant: None,
            profile: Some(OsString::from(DIST_PROFILE)),
        }
    }
}

impl From<PackageArgs> for PackageOptions {
    fn from(args: PackageArgs) -> Self {
        // Determine which package types to build (default: all if none specified)
        let (build_deb, build_rpm, build_tarball) = if !args.deb && !args.rpm && !args.tarball {
            (true, true, true)
        } else {
            (args.deb, args.rpm, args.tarball)
        };

        // Determine profile: explicit flags take precedence, default to dist
        let profile = if args.no_profile {
            None
        } else if args.debug {
            Some(OsString::from("debug"))
        } else if let Some(name) = args.profile {
            Some(OsString::from(name))
        } else {
            // --release is implicit default
            Some(OsString::from(DIST_PROFILE))
        };

        Self {
            build_deb,
            build_rpm,
            build_tarball,
            tarball_target: args.tarball_target.map(OsString::from),
            deb_variant: args.deb_variant,
            profile,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PackageArgs;

    #[test]
    fn from_args_default_builds_all_with_dist_profile() {
        let args = PackageArgs::default();
        let options: PackageOptions = args.into();
        assert_eq!(options, PackageOptions::release_all());
    }

    #[test]
    fn from_args_tarball_only() {
        let args = PackageArgs {
            tarball: true,
            ..Default::default()
        };
        let options: PackageOptions = args.into();
        assert_eq!(
            options,
            PackageOptions {
                build_deb: false,
                build_rpm: false,
                build_tarball: true,
                tarball_target: None,
                deb_variant: None,
                profile: Some(OsString::from(DIST_PROFILE)),
            }
        );
    }

    #[test]
    fn from_args_tarball_with_target() {
        let args = PackageArgs {
            tarball: true,
            tarball_target: Some("x86_64-unknown-linux-gnu".to_owned()),
            ..Default::default()
        };
        let options: PackageOptions = args.into();
        assert_eq!(
            options,
            PackageOptions {
                build_deb: false,
                build_rpm: false,
                build_tarball: true,
                tarball_target: Some(OsString::from("x86_64-unknown-linux-gnu")),
                deb_variant: None,
                profile: Some(OsString::from(DIST_PROFILE)),
            }
        );
    }

    #[test]
    fn from_args_debug_profile() {
        let args = PackageArgs {
            debug: true,
            ..Default::default()
        };
        let options: PackageOptions = args.into();
        assert_eq!(options.profile, Some(OsString::from("debug")));
    }

    #[test]
    fn from_args_custom_profile() {
        let args = PackageArgs {
            profile: Some("custom".to_owned()),
            ..Default::default()
        };
        let options: PackageOptions = args.into();
        assert_eq!(options.profile, Some(OsString::from("custom")));
    }

    #[test]
    fn from_args_no_profile() {
        let args = PackageArgs {
            no_profile: true,
            ..Default::default()
        };
        let options: PackageOptions = args.into();
        assert_eq!(options.profile, None);
    }

    #[test]
    fn from_args_deb_variant() {
        let args = PackageArgs {
            deb: true,
            deb_variant: Some("focal".to_owned()),
            ..Default::default()
        };
        let options: PackageOptions = args.into();
        assert_eq!(options.deb_variant, Some("focal".to_owned()));
        assert!(options.build_deb);
    }

    #[test]
    fn release_all_returns_dist_profile_with_all_targets() {
        let options = PackageOptions::release_all();
        assert!(options.build_deb);
        assert!(options.build_rpm);
        assert!(options.build_tarball);
        assert_eq!(options.deb_variant, None);
        assert_eq!(options.profile, Some(OsString::from(DIST_PROFILE)));
    }
}
