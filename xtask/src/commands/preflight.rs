use crate::error::{TaskError, TaskResult};
use crate::util::{cargo_metadata_json, ensure, is_help_flag, validation_error};
use crate::workspace::{
    WorkspaceBranding, parse_workspace_branding_from_value, read_workspace_manifest,
};
use serde_json::Value as JsonValue;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use toml::Value;

/// Options accepted by the `preflight` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreflightOptions;

/// Parses CLI arguments for the `preflight` command.
pub fn parse_args<I>(args: I) -> TaskResult<PreflightOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for preflight command",
            arg.to_string_lossy()
        )));
    }

    Ok(PreflightOptions)
}

/// Executes the `preflight` command.
pub fn execute(workspace: &Path, _options: PreflightOptions) -> TaskResult<()> {
    let manifest_text = read_workspace_manifest(workspace)?;
    let manifest_value = manifest_text.parse::<Value>().map_err(|error| {
        TaskError::Metadata(format!("failed to parse workspace manifest: {error}"))
    })?;
    let branding = parse_workspace_branding_from_value(&manifest_value)?;

    validate_branding(&branding)?;
    validate_packaging_assets(workspace, &branding)?;
    validate_package_versions(workspace, &branding)?;
    validate_workspace_package_rust_version(&manifest_value)?;
    validate_documentation(workspace, &branding)?;

    println!(
        "Preflight checks passed: branding, version, packaging metadata, documentation, and toolchain requirements validated."
    );

    Ok(())
}

fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
    ensure(
        branding.brand == "oc",
        format!("workspace brand must be 'oc', found {:?}", branding.brand),
    )?;
    ensure(
        branding.upstream_version == "3.4.1",
        format!(
            "upstream_version must remain aligned with rsync 3.4.1; found {:?}",
            branding.upstream_version
        ),
    )?;
    ensure(
        branding.rust_version.ends_with("-rust"),
        format!(
            "Rust-branded version should end with '-rust'; found {:?}",
            branding.rust_version
        ),
    )?;
    ensure(
        branding.protocol == 32,
        format!("Supported protocol must be 32; found {}", branding.protocol),
    )?;
    ensure(
        branding.client_bin.starts_with("oc-"),
        format!(
            "client_bin must start with 'oc-'; found {:?}",
            branding.client_bin
        ),
    )?;
    ensure(
        branding.daemon_bin.starts_with("oc-"),
        format!(
            "daemon_bin must start with 'oc-'; found {:?}",
            branding.daemon_bin
        ),
    )?;

    let config_dir = Path::new(&branding.daemon_config_dir);
    ensure(
        config_dir.is_absolute(),
        format!(
            "daemon_config_dir must be an absolute path; found {}",
            branding.daemon_config_dir
        ),
    )?;

    let config_path = Path::new(&branding.daemon_config);
    let secrets_path = Path::new(&branding.daemon_secrets);
    ensure(
        config_path.is_absolute(),
        format!(
            "daemon_config must be an absolute path; found {}",
            branding.daemon_config
        ),
    )?;
    ensure(
        secrets_path.is_absolute(),
        format!(
            "daemon_secrets must be an absolute path; found {}",
            branding.daemon_secrets
        ),
    )?;

    ensure(
        config_path.parent() == Some(config_dir),
        format!(
            "daemon_config {} must reside within configured directory {}",
            branding.daemon_config, branding.daemon_config_dir
        ),
    )?;
    ensure(
        secrets_path.parent() == Some(config_dir),
        format!(
            "daemon_secrets {} must reside within configured directory {}",
            branding.daemon_secrets, branding.daemon_config_dir
        ),
    )?;

    ensure(
        config_path.file_name() != secrets_path.file_name(),
        "daemon configuration and secrets paths must not collide",
    )?;

    Ok(())
}

fn validate_packaging_assets(workspace: &Path, branding: &WorkspaceBranding) -> TaskResult<()> {
    let packaging_root = workspace.join("packaging").join("etc").join("oc-rsyncd");
    let config_name = Path::new(&branding.daemon_config)
        .file_name()
        .ok_or_else(|| validation_error("daemon_config must include a file name"))?;
    let secrets_name = Path::new(&branding.daemon_secrets)
        .file_name()
        .ok_or_else(|| validation_error("daemon_secrets must include a file name"))?;

    let assets = [
        (config_name, "daemon_config"),
        (secrets_name, "daemon_secrets"),
    ];

    for (name, label) in assets {
        let candidate = packaging_root.join(name);
        ensure(
            candidate.exists(),
            format!(
                "packaging assets missing for {} (expected {})",
                label,
                candidate.display()
            ),
        )?;
    }

    let systemd_unit = workspace
        .join("packaging")
        .join("systemd")
        .join("oc-rsyncd.service");
    let unit_contents = fs::read_to_string(&systemd_unit).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!("failed to read {}: {}", systemd_unit.display(), error),
        ))
    })?;

    let unit_snippets = [
        branding.daemon_bin.as_str(),
        branding.daemon_config.as_str(),
        branding.daemon_secrets.as_str(),
        "Description=oc-rsyncd",
        "Alias=rsyncd.service",
        "OC_RSYNC_CONFIG",
        "OC_RSYNC_SECRETS",
        "RSYNCD_CONFIG",
        "RSYNCD_SECRETS",
    ];

    for snippet in unit_snippets {
        ensure(
            unit_contents.contains(snippet),
            format!(
                "systemd unit {} missing required snippet '{}': update packaging/systemd/oc-rsyncd.service",
                systemd_unit.display(),
                snippet
            ),
        )?;
    }

    let env_file = workspace
        .join("packaging")
        .join("default")
        .join("oc-rsyncd");
    let env_contents = fs::read_to_string(&env_file).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!("failed to read {}: {}", env_file.display(), error),
        ))
    })?;

    let env_snippets = [
        "OC_RSYNC_CONFIG",
        "RSYNCD_CONFIG",
        "OC_RSYNC_SECRETS",
        "RSYNCD_SECRETS",
    ];

    for snippet in env_snippets {
        ensure(
            env_contents.contains(snippet),
            format!(
                "environment defaults {} missing '{}': update packaging/default/oc-rsyncd",
                env_file.display(),
                snippet
            ),
        )?;
    }

    Ok(())
}

fn validate_package_versions(workspace: &Path, branding: &WorkspaceBranding) -> TaskResult<()> {
    let metadata = cargo_metadata_json(workspace)?;
    let packages = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| validation_error("cargo metadata output missing packages array"))?;

    let mut versions = std::collections::HashMap::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(version) = package.get("version").and_then(JsonValue::as_str) else {
            continue;
        };
        versions.insert(name.to_string(), version.to_string());
    }

    for crate_name in ["oc-rsync-bin", "oc-rsyncd-bin"] {
        let version = versions.get(crate_name).ok_or_else(|| {
            validation_error(format!("crate {crate_name} missing from cargo metadata"))
        })?;
        ensure(
            version == &branding.rust_version,
            format!(
                "crate {crate_name} version {version} does not match {}",
                branding.rust_version
            ),
        )?;
    }

    Ok(())
}

fn validate_workspace_package_rust_version(manifest: &Value) -> TaskResult<()> {
    let workspace = manifest
        .get("workspace")
        .and_then(Value::as_table)
        .ok_or_else(|| validation_error("missing [workspace] table in Cargo.toml"))?;
    let package = workspace
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(|| validation_error("missing [workspace.package] table in Cargo.toml"))?;
    let rust_version = package
        .get("rust-version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            validation_error("workspace.package.rust-version missing from Cargo.toml")
        })?;

    ensure(
        rust_version == "1.87",
        format!(
            "workspace.package.rust-version must match CI toolchain 1.87; found {:?}",
            rust_version
        ),
    )
}

fn validate_documentation(workspace: &Path, branding: &WorkspaceBranding) -> TaskResult<()> {
    struct DocumentationCheck<'a> {
        relative_path: &'a str,
        required_snippets: Vec<&'a str>,
    }

    let checks = [
        DocumentationCheck {
            relative_path: "README.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/production_scope_p1.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config_dir.as_str(),
                branding.daemon_config.as_str(),
                branding.daemon_secrets.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/differences.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/gaps.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/COMPARE.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
            ],
        },
    ];

    for check in checks {
        let path = workspace.join(check.relative_path);
        let contents = fs::read_to_string(&path).map_err(|error| {
            TaskError::Io(std::io::Error::new(
                error.kind(),
                format!("failed to read {}: {error}", path.display()),
            ))
        })?;

        let missing: Vec<&str> = check
            .required_snippets
            .iter()
            .copied()
            .filter(|snippet| !snippet.is_empty() && !contents.contains(snippet))
            .collect();

        ensure(
            missing.is_empty(),
            format!(
                "{} missing required documentation snippets: {}",
                check.relative_path,
                missing
                    .iter()
                    .map(|snippet| format!("'{}'", snippet))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )?;
    }

    Ok(())
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask preflight\n\nOptions:\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, PreflightOptions);
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("preflight")));
    }
}
