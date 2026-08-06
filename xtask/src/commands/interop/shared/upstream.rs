//! Upstream rsync binary detection and management.

use crate::error::{TaskError, TaskResult};
use std::path::{Path, PathBuf};

/// Supported upstream rsync versions for interop testing.
pub const UPSTREAM_VERSIONS: &[&str] = &["3.0.9", "3.1.3", "3.4.1"];

/// Root under which `tools/ci/run_interop.sh` installs the pinned upstream
/// builds, one `<version>/bin/rsync` per entry. This is the single place any
/// xtask command may look for an upstream binary.
pub fn install_root(workspace: &Path) -> PathBuf {
    workspace.join("target/interop/upstream-install")
}

/// Path of the pinned upstream build for `version` under `install_root`.
pub fn pinned_binary(install_root: &Path, version: &str) -> PathBuf {
    install_root.join(version).join("bin/rsync")
}

/// First line of `binary --version`, or `None` if it cannot be run.
pub fn version_banner(binary: &Path) -> Option<String> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.lines().next().map(|l| l.trim_end().to_owned())
}

/// Dotted release version parsed from an rsync `--version` banner line.
///
/// The install path is not authoritative: a directory named `3.4.4` may hold
/// any binary, and `/usr/bin/rsync` on macOS is openrsync. Mirrors
/// `upstream_release_version()` in `tools/ci/run_interop.sh`, which parses the
/// banner for the same reason.
pub fn parse_release_version(banner: &str) -> Option<&str> {
    let rest = banner.strip_prefix("rsync")?.trim_start();
    let rest = rest.strip_prefix("version")?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let version = &rest[..end];
    version
        .starts_with(|c: char| c.is_ascii_digit())
        .then_some(version)
}

/// Detect available upstream rsync binaries.
pub fn detect_upstream_binaries(workspace: &Path) -> TaskResult<Vec<UpstreamBinary>> {
    let interop_dir = install_root(workspace);

    let mut binaries = Vec::new();

    for version in UPSTREAM_VERSIONS {
        let binary_path = pinned_binary(&interop_dir, version);

        if binary_path.exists() {
            binaries.push(UpstreamBinary {
                version: (*version).to_owned(),
                path: binary_path,
            });
        }
    }

    if binaries.is_empty() {
        return Err(TaskError::ToolMissing(format!(
            "No upstream rsync binaries found in {}\n\
             Run 'bash tools/ci/run_interop.sh' first to build upstream versions.",
            interop_dir.display()
        )));
    }

    Ok(binaries)
}

/// Information about an upstream rsync binary.
#[derive(Debug, Clone)]
pub struct UpstreamBinary {
    /// Version string (e.g., "3.4.1").
    pub version: String,
    /// Path to the binary.
    pub path: PathBuf,
}

impl UpstreamBinary {
    /// Check if this binary exists and is executable.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        self.path.exists() && self.path.is_file()
    }

    /// Get version string for display.
    #[allow(dead_code)]
    pub fn version_string(&self) -> &str {
        &self.version
    }

    /// Get path to the binary.
    pub fn binary_path(&self) -> &Path {
        &self.path
    }
}
