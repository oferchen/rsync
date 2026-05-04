//! Generate and package rustdoc for distribution.
//!
//! This command builds the rustdoc documentation for the workspace and packages
//! it into a tarball suitable for hosting or distribution with releases.

use crate::cli::DocPackageArgs;
use crate::error::TaskError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Options for doc packaging
#[derive(Debug)]
pub struct DocPackageOptions {
    /// Output directory for the documentation tarball
    pub output: PathBuf,
    /// Whether to open the documentation in a browser after building
    pub open: bool,
}

impl Default for DocPackageOptions {
    fn default() -> Self {
        Self {
            output: PathBuf::from("target/doc-dist"),
            open: false,
        }
    }
}

impl From<DocPackageArgs> for DocPackageOptions {
    fn from(args: DocPackageArgs) -> Self {
        Self {
            output: args.output,
            open: args.open,
        }
    }
}

/// Execute doc packaging
pub fn execute(workspace: &Path, options: DocPackageOptions) -> Result<(), TaskError> {
    println!("Building API documentation...");

    // Build rustdoc with --no-deps to focus on workspace crates
    let mut cmd = Command::new("cargo");
    cmd.arg("doc")
        .arg("--workspace")
        .arg("--no-deps")
        .arg("--all-features")
        .current_dir(workspace);

    if options.open {
        cmd.arg("--open");
    }

    let status = cmd.status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: "cargo doc".to_owned(),
            status,
        });
    }

    println!("Documentation built successfully");

    // Create output directory
    std::fs::create_dir_all(&options.output)?;

    // Package documentation
    let doc_dir = workspace.join("target/doc");
    let tarball_name = "oc-rsync-rustdoc.tar.gz";
    let tarball_path = options.output.join(tarball_name);

    println!("Packaging documentation to {}...", tarball_path.display());

    let status = Command::new("tar")
        .args([
            "-czf",
            tarball_path.to_str().unwrap(),
            "-C",
            doc_dir.parent().unwrap().to_str().unwrap(),
            "doc",
        ])
        .current_dir(workspace)
        .status()?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: "tar".to_owned(),
            status,
        });
    }

    println!("✓ Documentation packaged: {}", tarball_path.display());
    println!();
    println!("To extract and view:");
    println!("  tar -xzf {}", tarball_path.display());
    println!("  open doc/core/index.html");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_args_uses_specified_output() {
        let args = DocPackageArgs {
            output: PathBuf::from("custom/path"),
            open: false,
        };
        let options: DocPackageOptions = args.into();
        assert_eq!(options.output, PathBuf::from("custom/path"));
        assert!(!options.open);
    }

    #[test]
    fn from_args_with_open_flag() {
        let args = DocPackageArgs {
            output: PathBuf::from("target/doc-dist"),
            open: true,
        };
        let options: DocPackageOptions = args.into();
        assert!(options.open);
    }
}
