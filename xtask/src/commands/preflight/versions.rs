use crate::error::TaskResult;
use crate::util::{cargo_metadata_json, ensure, validation_error};
use crate::workspace::WorkspaceBranding;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::Path;
use toml::Value;

pub(super) fn validate_package_versions(
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

pub(super) fn validate_workspace_package_rust_version(manifest: &Value) -> TaskResult<()> {
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
