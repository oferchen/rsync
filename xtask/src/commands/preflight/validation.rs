use crate::error::{TaskError, TaskResult};
use crate::util::{cargo_metadata_json, ensure, validation_error};
use crate::workspace::WorkspaceBranding;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;

mod packaging;

use packaging::file_name;
pub(crate) use packaging::validate_packaging_assets;

pub(crate) fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
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

    let config_dir = branding.daemon_config_dir.as_path();
    ensure(
        config_dir.is_absolute(),
        format!(
            "daemon_config_dir must be an absolute path; found {}",
            branding.daemon_config_dir.display()
        ),
    )?;

    let config_path = branding.daemon_config.as_path();
    let secrets_path = branding.daemon_secrets.as_path();
    ensure(
        config_path.is_absolute(),
        format!(
            "daemon_config must be an absolute path; found {}",
            branding.daemon_config.display()
        ),
    )?;
    ensure(
        secrets_path.is_absolute(),
        format!(
            "daemon_secrets must be an absolute path; found {}",
            branding.daemon_secrets.display()
        ),
    )?;

    let expected_dir_suffix = format!("{}-rsyncd", branding.brand);
    ensure(
        config_dir.file_name().and_then(|name| name.to_str()) == Some(expected_dir_suffix.as_str()),
        format!(
            "daemon_config_dir must end with '{}'; found {}",
            expected_dir_suffix,
            branding.daemon_config_dir.display()
        ),
    )?;

    ensure(
        file_name(branding.daemon_config.as_path(), "daemon_config")?
            == PathBuf::from(format!("{}-rsyncd.conf", branding.brand)),
        format!(
            "daemon_config {} must be named {}-rsyncd.conf",
            branding.daemon_config.display(),
            branding.brand
        ),
    )?;
    ensure(
        file_name(branding.daemon_secrets.as_path(), "daemon_secrets")?
            == PathBuf::from(format!("{}-rsyncd.secrets", branding.brand)),
        format!(
            "daemon_secrets {} must be named {}-rsyncd.secrets",
            branding.daemon_secrets.display(),
            branding.brand
        ),
    )?;
    ensure(
        file_name(
            branding.legacy_daemon_config.as_path(),
            "legacy_daemon_config",
        )? == PathBuf::from("rsyncd.conf"),
        format!(
            "legacy_daemon_config {} must be named rsyncd.conf",
            branding.legacy_daemon_config.display()
        ),
    )?;
    ensure(
        file_name(
            branding.legacy_daemon_secrets.as_path(),
            "legacy_daemon_secrets",
        )? == PathBuf::from("rsyncd.secrets"),
        format!(
            "legacy_daemon_secrets {} must be named rsyncd.secrets",
            branding.legacy_daemon_secrets.display()
        ),
    )?;

    ensure(
        config_path.parent() == Some(config_dir),
        format!(
            "daemon_config {} must reside within configured directory {}",
            branding.daemon_config.display(),
            branding.daemon_config_dir.display()
        ),
    )?;
    ensure(
        secrets_path.parent() == Some(config_dir),
        format!(
            "daemon_secrets {} must reside within configured directory {}",
            branding.daemon_secrets.display(),
            branding.daemon_config_dir.display()
        ),
    )?;

    ensure(
        config_path.file_name() != secrets_path.file_name(),
        "daemon configuration and secrets paths must not collide",
    )?;

    Ok(())
}

pub(crate) fn validate_package_versions(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
    let metadata = cargo_metadata_json(workspace)?;
    let packages = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| validation_error("cargo metadata output missing packages array"))?;

    let mut versions = HashMap::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(version) = package.get("version").and_then(JsonValue::as_str) else {
            continue;
        };
        versions.insert(name.to_string(), version.to_string());
    }

    for crate_name in ["oc-rsync"] {
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

pub(crate) fn validate_workspace_package_rust_version(manifest: &Value) -> TaskResult<()> {
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

pub(crate) fn validate_documentation(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
    struct DocumentationCheck<'a> {
        relative_path: &'a str,
        required_snippets: Vec<String>,
    }

    let checks = [
        DocumentationCheck {
            relative_path: "README.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.rust_version.clone(),
                branding.daemon_config.display().to_string(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/production_scope_p1.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.rust_version.clone(),
                branding.daemon_config_dir.display().to_string(),
                branding.daemon_config.display().to_string(),
                branding.daemon_secrets.display().to_string(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/differences.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.rust_version.clone(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/gaps.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.rust_version.clone(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/COMPARE.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.rust_version.clone(),
                branding.daemon_config.display().to_string(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/feature_matrix.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.rust_version.clone(),
                branding.daemon_config.display().to_string(),
                branding.daemon_secrets.display().to_string(),
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
            .map(|snippet| snippet.as_str())
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
