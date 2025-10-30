use crate::error::{TaskError, TaskResult};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

/// Branding metadata extracted from the workspace manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceBranding {
    /// Short brand label (e.g. `"oc"`).
    pub brand: String,
    /// Upstream rsync version identifier (`"3.4.1"`).
    pub upstream_version: String,
    /// Rust-branded version identifier (`"3.4.1-rust"`).
    pub rust_version: String,
    /// Highest protocol version supported by the build.
    pub protocol: u16,
    /// Canonical client binary name.
    pub client_bin: String,
    /// Canonical daemon binary name.
    pub daemon_bin: String,
    /// Legacy upstream-compatible client name.
    pub legacy_client_bin: String,
    /// Legacy upstream-compatible daemon name.
    pub legacy_daemon_bin: String,
    /// Directory that houses daemon configuration files.
    pub daemon_config_dir: PathBuf,
    /// Primary daemon configuration file path.
    pub daemon_config: PathBuf,
    /// Primary daemon secrets file path.
    pub daemon_secrets: PathBuf,
    /// Legacy daemon configuration directory used by upstream-compatible installations.
    pub legacy_daemon_config_dir: PathBuf,
    /// Legacy daemon configuration file path used by upstream-compatible installations.
    pub legacy_daemon_config: PathBuf,
    /// Legacy daemon secrets file path used by upstream-compatible installations.
    pub legacy_daemon_secrets: PathBuf,
    /// Project source URL advertised in documentation and banners.
    pub source: String,
}

impl WorkspaceBranding {
    /// Returns a concise human-readable summary.
    pub fn summary(&self) -> String {
        format!(
            "brand={} rust_version={} protocol={} client={} daemon={} config_dir={} config={} secrets={}",
            self.brand,
            self.rust_version,
            self.protocol,
            self.client_bin,
            self.daemon_bin,
            self.daemon_config_dir.display(),
            self.daemon_config.display(),
            self.daemon_secrets.display()
        )
    }
}

/// Resolves the workspace root directory.
pub fn workspace_root() -> TaskResult<PathBuf> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        TaskError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "CARGO_MANIFEST_DIR environment variable is not set",
        ))
    })?;
    let mut path = PathBuf::from(manifest_dir);
    if !path.pop() {
        return Err(TaskError::Io(std::io::Error::other(
            "failed to locate workspace root",
        )));
    }
    Ok(path)
}

/// Builds the path to the workspace manifest.
pub fn workspace_manifest_path(workspace: &Path) -> PathBuf {
    workspace.join("Cargo.toml")
}

/// Reads the workspace manifest into memory.
pub fn read_workspace_manifest(workspace: &Path) -> TaskResult<String> {
    let manifest_path = workspace_manifest_path(workspace);
    fs::read_to_string(&manifest_path).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!(
                "failed to read workspace manifest at {}: {error}",
                manifest_path.display()
            ),
        ))
    })
}

/// Loads the workspace branding metadata from the manifest on disk.
pub fn load_workspace_branding(workspace: &Path) -> TaskResult<WorkspaceBranding> {
    let manifest = read_workspace_manifest(workspace)?;
    parse_workspace_branding(&manifest)
}

/// Parses the branding metadata from the provided manifest contents.
pub fn parse_workspace_branding(manifest: &str) -> TaskResult<WorkspaceBranding> {
    let value = manifest.parse::<Value>().map_err(|error| {
        TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;
    parse_workspace_branding_from_value(&value)
}

/// Parses the branding metadata from a TOML [`Value`].
pub fn parse_workspace_branding_from_value(value: &Value) -> TaskResult<WorkspaceBranding> {
    let workspace = value
        .get("workspace")
        .ok_or_else(|| metadata_error("missing [workspace] table"))?;
    let metadata = workspace
        .get("metadata")
        .ok_or_else(|| metadata_error("missing [workspace.metadata] table"))?;
    let oc = metadata
        .get("oc_rsync")
        .ok_or_else(|| metadata_error("missing [workspace.metadata.oc_rsync] table"))?;

    Ok(WorkspaceBranding {
        brand: metadata_str(oc, "brand")?,
        upstream_version: metadata_str(oc, "upstream_version")?,
        rust_version: metadata_str(oc, "rust_version")?,
        protocol: metadata_protocol(oc)?,
        client_bin: metadata_str(oc, "client_bin")?,
        daemon_bin: metadata_str(oc, "daemon_bin")?,
        legacy_client_bin: metadata_str(oc, "legacy_client_bin")?,
        legacy_daemon_bin: metadata_str(oc, "legacy_daemon_bin")?,
        daemon_config_dir: metadata_path(oc, "daemon_config_dir")?,
        daemon_config: metadata_path(oc, "daemon_config")?,
        daemon_secrets: metadata_path(oc, "daemon_secrets")?,
        legacy_daemon_config_dir: metadata_path(oc, "legacy_daemon_config_dir")?,
        legacy_daemon_config: metadata_path(oc, "legacy_daemon_config")?,
        legacy_daemon_secrets: metadata_path(oc, "legacy_daemon_secrets")?,
        source: metadata_str(oc, "source")?,
    })
}

fn metadata_str(table: &Value, key: &str) -> TaskResult<String> {
    table
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| metadata_error(format!("missing or non-string metadata field '{key}'")))
}

fn metadata_path(table: &Value, key: &str) -> TaskResult<PathBuf> {
    Ok(PathBuf::from(metadata_str(table, key)?))
}

fn metadata_protocol(table: &Value) -> TaskResult<u16> {
    let value = table
        .get("protocol")
        .and_then(Value::as_integer)
        .ok_or_else(|| metadata_error("missing or non-integer metadata field 'protocol'"))?;
    u16::try_from(value).map_err(|_| metadata_error("protocol value must fit into u16"))
}

fn metadata_error(message: impl Into<String>) -> TaskError {
    TaskError::Metadata(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_workspace_branding_extracts_fields() {
        let manifest = include_str!("../../Cargo.toml");
        let branding = parse_workspace_branding(manifest).expect("parse succeeds");
        let expected = WorkspaceBranding {
            brand: String::from("oc"),
            upstream_version: String::from("3.4.1"),
            rust_version: String::from("3.4.1-rust"),
            protocol: 32,
            client_bin: String::from("oc-rsync"),
            daemon_bin: String::from("oc-rsyncd"),
            legacy_client_bin: String::from("rsync"),
            legacy_daemon_bin: String::from("rsyncd"),
            daemon_config_dir: PathBuf::from("/etc/oc-rsyncd"),
            daemon_config: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
            daemon_secrets: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
            legacy_daemon_config_dir: PathBuf::from("/etc"),
            legacy_daemon_config: PathBuf::from("/etc/rsyncd.conf"),
            legacy_daemon_secrets: PathBuf::from("/etc/rsyncd.secrets"),
            source: String::from("https://github.com/oferchen/rsync"),
        };
        assert_eq!(branding, expected);
    }

    #[test]
    fn parse_workspace_branding_reports_missing_tables() {
        let manifest = "[workspace]\n[workspace.metadata]\n";
        let error = parse_workspace_branding(manifest).unwrap_err();
        assert!(matches!(error, TaskError::Metadata(_)));
    }
}
