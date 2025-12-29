//! Generate and package rustdoc for distribution.
//!
//! This command builds the rustdoc documentation for the workspace and packages
//! it into a tarball suitable for hosting or distribution with releases.

use crate::error::TaskError;
#[cfg(test)]
use crate::util::is_help_flag;
#[cfg(test)]
use std::ffi::OsString;
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

/// Parse command-line arguments for doc packaging
#[cfg(test)]
#[allow(dead_code)]
pub fn parse_args<I>(args: I) -> Result<DocPackageOptions, TaskError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut opts = DocPackageOptions::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        let arg_str = arg.to_string_lossy();
        match arg_str.as_ref() {
            "--output" | "-o" => {
                let Some(path) = iter.next() else {
                    return Err(TaskError::Usage(format!(
                        "{arg_str} requires a path argument"
                    )));
                };
                opts.output = PathBuf::from(path);
            }
            "--open" => {
                opts.open = true;
            }
            _ => {
                return Err(TaskError::Usage(format!("unknown flag: {arg_str}")));
            }
        }
    }

    Ok(opts)
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
#[allow(dead_code)]
fn usage() -> String {
    String::from(
        r#"cargo xtask doc-package

Generate and package rustdoc for distribution.

This command:
1. Builds rustdoc for all workspace crates with --no-deps
2. Creates a tarball of the documentation
3. Places the tarball in target/doc-dist/ (or custom --output path)

The resulting tarball can be:
- Uploaded to GitHub releases
- Hosted on a static web server
- Distributed with packages
- Extracted locally for offline browsing

OPTIONS:
    --output, -o <PATH>    Output directory for tarball [default: target/doc-dist]
    --open                 Open documentation in browser after building
    --help, -h             Show this help message

EXAMPLES:
    # Generate and package documentation
    cargo xtask doc-package

    # Build, package, and open in browser
    cargo xtask doc-package --open

    # Package to custom location
    cargo xtask doc-package --output artifacts/docs

INTEGRATION WITH CI:
    # In .github/workflows/release.yml:
    - name: Package documentation
      run: cargo xtask doc-package --output dist/

    - name: Upload documentation
      uses: actions/upload-artifact@v3
      with:
        name: rustdoc
        path: dist/oc-rsync-rustdoc.tar.gz"#,
    )
}
