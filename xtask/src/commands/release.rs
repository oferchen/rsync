mod upload;

use crate::cli::ReleaseArgs;
use crate::commands::{
    docs::{self, DocsOptions},
    enforce_limits::{self, EnforceLimitsOptions},
    no_binaries, no_placeholders,
    package::{self, PackageOptions},
    preflight, readme_version,
};
use crate::error::TaskResult;
use crate::workspace::load_workspace_branding;
use std::path::Path;
use upload::upload_release_artifacts;

/// Options accepted by the `release` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReleaseOptions {
    /// Skip rebuilding API documentation and doctests.
    pub skip_docs: bool,
    /// Skip source line-count enforcement checks.
    pub skip_hygiene: bool,
    /// Skip placeholder scans for Rust sources.
    pub skip_placeholder_scan: bool,
    /// Skip auditing the git index for tracked binary artifacts.
    pub skip_binary_scan: bool,
    /// Skip building distributable packages.
    pub skip_packages: bool,
    /// Skip uploading release artifacts to GitHub.
    pub skip_upload: bool,
}

impl From<ReleaseArgs> for ReleaseOptions {
    fn from(args: ReleaseArgs) -> Self {
        Self {
            skip_docs: args.skip_docs,
            skip_hygiene: args.skip_hygiene,
            skip_placeholder_scan: args.skip_placeholder_scan,
            skip_binary_scan: args.skip_binary_scan,
            skip_packages: args.skip_packages,
            skip_upload: args.skip_upload,
        }
    }
}

/// Executes the `release` command.
pub fn execute(workspace: &Path, options: ReleaseOptions) -> TaskResult<()> {
    let mut executed_steps = Vec::new();
    let mut skipped_steps = Vec::new();

    if options.skip_binary_scan {
        skipped_steps.push("no-binaries");
    } else {
        no_binaries::execute(workspace)?;
        executed_steps.push("no-binaries");
    }

    preflight::execute(workspace)?;
    executed_steps.push("preflight");

    readme_version::execute(workspace)?;
    executed_steps.push("readme-version");

    if options.skip_docs {
        skipped_steps.push("docs");
    } else {
        let docs_options = DocsOptions {
            validate: true,
            ..DocsOptions::default()
        };
        docs::execute(workspace, docs_options)?;
        executed_steps.push("docs");
    }

    if options.skip_hygiene {
        skipped_steps.push("enforce-limits");
    } else {
        enforce_limits::execute(workspace, EnforceLimitsOptions::default())?;
        executed_steps.push("enforce-limits");
    }

    if options.skip_placeholder_scan {
        skipped_steps.push("no-placeholders");
    } else {
        no_placeholders::execute(workspace)?;
        executed_steps.push("no-placeholders");
    }

    if options.skip_packages {
        skipped_steps.push("package");
    } else {
        let package_options = PackageOptions::release_all();
        package::execute(workspace, package_options)?;
        executed_steps.push("package");
    }

    if options.skip_upload {
        skipped_steps.push("upload");
    } else {
        let branding = load_workspace_branding(workspace)?;
        upload_release_artifacts(workspace, &branding)?;
        executed_steps.push("upload");
    }

    if skipped_steps.is_empty() {
        println!(
            "Release validation complete. Executed steps: {}.",
            executed_steps.join(", ")
        );
    } else {
        println!(
            "Release validation complete. Executed: {}; skipped: {}.",
            executed_steps.join(", "),
            skipped_steps.join(", ")
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ReleaseArgs;

    #[test]
    fn from_args_default_configuration() {
        let args = ReleaseArgs::default();
        let options: ReleaseOptions = args.into();
        assert_eq!(options, ReleaseOptions::default());
    }

    #[test]
    fn from_args_all_skip_flags() {
        let args = ReleaseArgs {
            skip_docs: true,
            skip_hygiene: true,
            skip_placeholder_scan: true,
            skip_binary_scan: true,
            skip_packages: true,
            skip_upload: true,
        };
        let options: ReleaseOptions = args.into();
        assert!(options.skip_docs);
        assert!(options.skip_hygiene);
        assert!(options.skip_placeholder_scan);
        assert!(options.skip_binary_scan);
        assert!(options.skip_packages);
        assert!(options.skip_upload);
    }

    #[test]
    fn from_args_partial_skip_flags() {
        let args = ReleaseArgs {
            skip_docs: true,
            skip_upload: true,
            ..Default::default()
        };
        let options: ReleaseOptions = args.into();
        assert!(options.skip_docs);
        assert!(!options.skip_hygiene);
        assert!(!options.skip_placeholder_scan);
        assert!(!options.skip_binary_scan);
        assert!(!options.skip_packages);
        assert!(options.skip_upload);
    }
}
