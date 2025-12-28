use crate::error::{TaskError, TaskResult};
use branding::workspace;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

/// Branding metadata extracted from the workspace manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceBranding {
    /// Short brand label (e.g. `"oc"`).
    pub brand: String,
    /// Upstream rsync version identifier (e.g., `"3.4.1"`).
    pub upstream_version: String,
    /// oc-rsync release version identifier (e.g., `"0.5.0"`).
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
    /// Cross-compilation targets grouped by operating system.
    pub cross_compile: BTreeMap<String, Vec<String>>,
    /// Expected matrix availability flags keyed by platform identifier.
    pub cross_compile_matrix: BTreeMap<String, bool>,
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

/// Returns the name of the root package defined in `Cargo.toml`.
pub fn root_package_name(workspace: &Path) -> TaskResult<String> {
    let manifest = read_workspace_manifest(workspace)?;
    let value = manifest.parse::<Value>().map_err(|error| {
        TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;

    let package_table = value
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(|| metadata_error("missing [package] table"))?;

    package_table
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| metadata_error("missing package.name entry"))
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

    let metadata = workspace::metadata();
    ensure_metadata_string(oc, "brand", metadata.brand())?;
    ensure_metadata_string(oc, "upstream_version", metadata.upstream_version())?;
    ensure_metadata_string(oc, "rust_version", metadata.rust_version())?;
    ensure_metadata_protocol(oc, metadata.protocol_version())?;
    ensure_metadata_string(oc, "client_bin", metadata.client_program_name())?;
    ensure_metadata_string(oc, "daemon_bin", metadata.daemon_program_name())?;
    ensure_metadata_string(
        oc,
        "legacy_client_bin",
        metadata.legacy_client_program_name(),
    )?;
    ensure_metadata_string(
        oc,
        "legacy_daemon_bin",
        metadata.legacy_daemon_program_name(),
    )?;
    ensure_metadata_string(oc, "daemon_config_dir", metadata.daemon_config_dir())?;
    ensure_metadata_string(oc, "daemon_config", metadata.daemon_config_path())?;
    ensure_metadata_string(oc, "daemon_secrets", metadata.daemon_secrets_path())?;
    ensure_metadata_string(
        oc,
        "legacy_daemon_config_dir",
        metadata.legacy_daemon_config_dir(),
    )?;
    ensure_metadata_string(
        oc,
        "legacy_daemon_config",
        metadata.legacy_daemon_config_path(),
    )?;
    ensure_metadata_string(
        oc,
        "legacy_daemon_secrets",
        metadata.legacy_daemon_secrets_path(),
    )?;
    ensure_metadata_string(oc, "source", metadata.source_url())?;

    let protocol = u16::try_from(metadata.protocol_version())
        .map_err(|_| metadata_error("protocol value must fit into u16"))?;

    Ok(WorkspaceBranding {
        brand: metadata.brand().to_owned(),
        upstream_version: metadata.upstream_version().to_owned(),
        rust_version: metadata.rust_version().to_owned(),
        protocol,
        client_bin: metadata.client_program_name().to_owned(),
        daemon_bin: metadata.daemon_program_name().to_owned(),
        legacy_client_bin: metadata.legacy_client_program_name().to_owned(),
        legacy_daemon_bin: metadata.legacy_daemon_program_name().to_owned(),
        daemon_config_dir: PathBuf::from(metadata.daemon_config_dir()),
        daemon_config: PathBuf::from(metadata.daemon_config_path()),
        daemon_secrets: PathBuf::from(metadata.daemon_secrets_path()),
        legacy_daemon_config_dir: PathBuf::from(metadata.legacy_daemon_config_dir()),
        legacy_daemon_config: PathBuf::from(metadata.legacy_daemon_config_path()),
        legacy_daemon_secrets: PathBuf::from(metadata.legacy_daemon_secrets_path()),
        source: metadata.source_url().to_owned(),
        cross_compile: metadata_cross_compile(oc)?,
        cross_compile_matrix: metadata_cross_compile_matrix(oc)?,
    })
}

fn ensure_metadata_string(table: &Value, key: &str, expected: &str) -> TaskResult<()> {
    match table.get(key) {
        Some(Value::String(value)) if value == expected => Ok(()),
        Some(Value::String(value)) => Err(metadata_error(format!(
            "metadata field '{key}' must be '{expected}' but was '{value}'"
        ))),
        Some(_) => Err(metadata_error(format!(
            "metadata field '{key}' must be a string"
        ))),
        None => Err(metadata_error(format!("missing metadata field '{key}'"))),
    }
}

fn ensure_metadata_protocol(table: &Value, expected: u32) -> TaskResult<()> {
    match table.get("protocol") {
        Some(Value::Integer(value)) if *value == expected as i64 => Ok(()),
        Some(Value::Integer(value)) => Err(metadata_error(format!(
            "metadata field 'protocol' must be {expected} but was {value}"
        ))),
        Some(_) => Err(metadata_error(
            "metadata field 'protocol' must be an integer",
        )),
        None => Err(metadata_error("missing metadata field 'protocol'")),
    }
}

fn metadata_cross_compile(table: &Value) -> TaskResult<BTreeMap<String, Vec<String>>> {
    let cross_compile = table
        .get("cross_compile")
        .ok_or_else(|| metadata_error("missing [workspace.metadata.oc_rsync.cross_compile] table"))?
        .as_table()
        .ok_or_else(|| metadata_error("cross_compile metadata must be a table"))?;

    let mut result = BTreeMap::new();
    for (os, value) in cross_compile {
        let list = value.as_array().ok_or_else(|| {
            metadata_error(format!("cross_compile entry '{os}' must be an array"))
        })?;

        if list.is_empty() {
            return Err(metadata_error(format!(
                "cross_compile entry '{os}' must contain at least one target"
            )));
        }

        let mut targets = Vec::with_capacity(list.len());
        let mut seen = BTreeSet::new();
        for entry in list {
            let target = entry.as_str().ok_or_else(|| {
                metadata_error(format!(
                    "cross_compile entry '{os}' must contain only strings"
                ))
            })?;
            if !seen.insert(target) {
                return Err(metadata_error(format!(
                    "cross_compile entry '{os}' contains duplicate target '{target}'"
                )));
            }
            targets.push(target.to_owned());
        }

        result.insert(os.to_owned(), targets);
    }

    if result.is_empty() {
        return Err(metadata_error(
            "cross_compile metadata must contain at least one platform",
        ));
    }

    Ok(result)
}

fn metadata_cross_compile_matrix(table: &Value) -> TaskResult<BTreeMap<String, bool>> {
    let Some(matrix_value) = table.get("cross_compile_matrix") else {
        return Ok(BTreeMap::new());
    };

    let matrix_table = matrix_value
        .as_table()
        .ok_or_else(|| metadata_error("cross_compile_matrix metadata must be a table"))?;

    let mut result = BTreeMap::new();
    for (platform, value) in matrix_table {
        let enabled = match value {
            Value::Boolean(flag) => *flag,
            Value::Table(inner) => inner
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    metadata_error(format!(
                        "cross_compile_matrix entry '{platform}' must provide a boolean 'enabled' field"
                    ))
                })?,
            _ => {
                return Err(metadata_error(format!(
                    "cross_compile_matrix entry '{platform}' must be a boolean or table"
                )))
            }
        };

        result.insert(platform.to_owned(), enabled);
    }

    Ok(result)
}

fn metadata_error(message: impl Into<String>) -> TaskError {
    TaskError::Metadata(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TaskError;
    use branding::workspace;

    fn manifest_with_cross_compile(entries: &str) -> String {
        let metadata = workspace::metadata();
        format!(
            r#"[workspace]
members = []

[workspace.metadata.oc_rsync]
brand = "{brand}"
upstream_version = "{upstream_version}"
rust_version = "{rust_version}"
protocol = {protocol}
client_bin = "{client_bin}"
daemon_bin = "{daemon_bin}"
legacy_client_bin = "{legacy_client_bin}"
legacy_daemon_bin = "{legacy_daemon_bin}"
daemon_config_dir = "{daemon_config_dir}"
daemon_config = "{daemon_config}"
daemon_secrets = "{daemon_secrets}"
legacy_daemon_config_dir = "{legacy_daemon_config_dir}"
legacy_daemon_config = "{legacy_daemon_config}"
legacy_daemon_secrets = "{legacy_daemon_secrets}"
source = "{source}"

[workspace.metadata.oc_rsync.cross_compile]
{entries}
"#,
            brand = metadata.brand(),
            upstream_version = metadata.upstream_version(),
            rust_version = metadata.rust_version(),
            protocol = metadata.protocol_version(),
            client_bin = metadata.client_program_name(),
            daemon_bin = metadata.daemon_program_name(),
            legacy_client_bin = metadata.legacy_client_program_name(),
            legacy_daemon_bin = metadata.legacy_daemon_program_name(),
            daemon_config_dir = metadata.daemon_config_dir(),
            daemon_config = metadata.daemon_config_path(),
            daemon_secrets = metadata.daemon_secrets_path(),
            legacy_daemon_config_dir = metadata.legacy_daemon_config_dir(),
            legacy_daemon_config = metadata.legacy_daemon_config_path(),
            legacy_daemon_secrets = metadata.legacy_daemon_secrets_path(),
            source = metadata.source_url(),
        )
    }

    // #[test]
    // fn parse_workspace_branding_extracts_fields() {
    //     let manifest = include_str!("../../Cargo.toml");
    //     let branding = parse_workspace_branding(manifest).expect("parse succeeds");
    //     let metadata = workspace::metadata();
    //     let expected = WorkspaceBranding {
    //         brand: metadata.brand().to_owned(),
    //         upstream_version: metadata.upstream_version().to_owned(),
    //         rust_version: metadata.rust_version().to_owned(),
    //         protocol: u16::try_from(metadata.protocol_version()).unwrap(),
    //         client_bin: metadata.client_program_name().to_owned(),
    //         daemon_bin: metadata.daemon_program_name().to_owned(),
    //         legacy_client_bin: metadata.legacy_client_program_name().to_owned(),
    //         legacy_daemon_bin: metadata.legacy_daemon_program_name().to_owned(),
    //         daemon_config_dir: PathBuf::from(metadata.daemon_config_dir()),
    //         daemon_config: PathBuf::from(metadata.daemon_config_path()),
    //         daemon_secrets: PathBuf::from(metadata.daemon_secrets_path()),
    //         legacy_daemon_config_dir: PathBuf::from(metadata.legacy_daemon_config_dir()),
    //         legacy_daemon_config: PathBuf::from(metadata.legacy_daemon_config_path()),
    //         legacy_daemon_secrets: PathBuf::from(metadata.legacy_daemon_secrets_path()),
    //         source: metadata.source_url().to_owned(),
    //         cross_compile: BTreeMap::from([
    //             (
    //                 String::from("linux"),
    //                 vec![String::from("x86_64"), String::from("aarch64")],
    //             ),
    //             (
    //                 String::from("macos"),
    //                 vec![String::from("x86_64"), String::from("aarch64")],
    //             ),
    //             (
    //                 String::from("windows"),
    //                 vec![String::from("x86_64"), String::from("aarch64")],
    //             ),
    //         ]),
    //         cross_compile_matrix: BTreeMap::from([
    //             (String::from("darwin-aarch64"), true),
    //             (String::from("darwin-x86_64"), true),
    //             (String::from("linux-aarch64"), true),
    //             (String::from("linux-x86_64"), true),
    //             (String::from("windows-aarch64"), false),
    //             (String::from("windows-x86"), false),
    //             (String::from("windows-x86_64"), true),
    //         ]),
    //     };
    //     assert_eq!(branding, expected);
    // }

    #[test]
    fn cross_compile_entries_must_not_be_empty() {
        let manifest = manifest_with_cross_compile("linux = []");
        let error = parse_workspace_branding(&manifest).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Metadata(message)
                if message.contains("cross_compile entry 'linux' must contain at least one target")
        ));
    }

    #[test]
    fn cross_compile_entries_reject_duplicates() {
        let manifest = manifest_with_cross_compile("linux = [\"x86_64\", \"x86_64\"]");
        let error = parse_workspace_branding(&manifest).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Metadata(message)
                if message.contains("cross_compile entry 'linux' contains duplicate target 'x86_64'")
        ));
    }

    #[test]
    fn parse_workspace_branding_reports_missing_tables() {
        let manifest = "[workspace]\\n[workspace.metadata]\\n";
        let error = parse_workspace_branding(manifest).unwrap_err();
        assert!(matches!(error, TaskError::Metadata(_)));
    }

    #[test]
    fn root_package_name_matches_manifest() {
        let workspace = super::workspace_root().expect("workspace root");
        let name = super::root_package_name(&workspace).expect("package name");
        assert_eq!(name, "bin");
    }
}
