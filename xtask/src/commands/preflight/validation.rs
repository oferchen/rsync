use crate::error::TaskResult;
use crate::util::{cargo_metadata_json, ensure, read_file_with_context, validation_error};
use crate::workspace::{self, WorkspaceBranding};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use toml::Value;

mod packaging;

use packaging::file_name;
pub(crate) use packaging::validate_packaging_assets;

/// Validates that workspace branding matches the pinned release invariants:
/// the upstream version compiled into the `branding` crate, protocol 33
/// (rsync 3.5.1 rsync.h:114 `PROTOCOL_VERSION`), brand-derived binary
/// and daemon file names, and absolute config/secrets paths within the
/// configured daemon directory.
pub(crate) fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
    let brand = branding.brand.trim();
    ensure(!brand.is_empty(), "workspace brand label must not be empty")?;
    let pinned_upstream = ::branding::branding::upstream_version();
    ensure(
        branding.upstream_version == pinned_upstream,
        format!(
            "upstream_version {:?} does not match the branding crate's {pinned_upstream:?}",
            branding.upstream_version
        ),
    )?;
    ensure(
        branding.protocol == 33,
        format!("Supported protocol must be 33; found {}", branding.protocol),
    )?;
    let expected_client = format!("{brand}-rsync");
    ensure(
        branding.client_bin == expected_client,
        format!(
            "client_bin must be '{expected_client}'; found {:?}",
            branding.client_bin
        ),
    )?;
    ensure(
        branding.daemon_bin == expected_client,
        format!(
            "daemon_bin must match client binary '{expected_client}'; found {:?}",
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

    let expected_dir_suffix = format!("{brand}-rsync");
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
            == Path::new(&format!("{brand}-rsyncd.conf")),
        format!(
            "daemon_config {} must be named {}-rsyncd.conf",
            branding.daemon_config.display(),
            brand
        ),
    )?;
    ensure(
        file_name(branding.daemon_secrets.as_path(), "daemon_secrets")?
            == Path::new(&format!("{brand}-rsyncd.secrets")),
        format!(
            "daemon_secrets {} must be named {}-rsyncd.secrets",
            branding.daemon_secrets.display(),
            brand
        ),
    )?;
    ensure(
        file_name(
            branding.legacy_daemon_config.as_path(),
            "legacy_daemon_config",
        )? == Path::new("rsyncd.conf"),
        format!(
            "legacy_daemon_config {} must be named rsyncd.conf",
            branding.legacy_daemon_config.display()
        ),
    )?;
    ensure(
        file_name(
            branding.legacy_daemon_secrets.as_path(),
            "legacy_daemon_secrets",
        )? == Path::new("rsyncd.secrets"),
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

/// Ensures the root crate version reported by `cargo metadata` matches the
/// branded `rust_version`.
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
        versions.insert(name.to_owned(), version.to_owned());
    }

    let crate_name = workspace::root_package_name(workspace)?;
    let version = versions.get(&crate_name).ok_or_else(|| {
        validation_error(format!("crate {crate_name} missing from cargo metadata"))
    })?;
    ensure(
        version == &branding.rust_version,
        format!(
            "crate {crate_name} version {version} does not match {}",
            branding.rust_version
        ),
    )?;

    Ok(())
}

/// Ensures `workspace.package.rust-version` in the manifest is the major.minor
/// of the toolchain pinned by `rust-toolchain.toml`, so the MSRV and the pin
/// move together.
pub(crate) fn validate_workspace_package_rust_version(
    workspace: &Path,
    manifest: &Value,
) -> TaskResult<()> {
    let workspace_table = manifest
        .get("workspace")
        .and_then(Value::as_table)
        .ok_or_else(|| validation_error("missing [workspace] table in Cargo.toml"))?;
    let package = workspace_table
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(|| validation_error("missing [workspace.package] table in Cargo.toml"))?;
    let rust_version = package
        .get("rust-version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            validation_error("workspace.package.rust-version missing from Cargo.toml")
        })?;

    let toolchain_path = workspace.join("rust-toolchain.toml");
    let toolchain = fs::read_to_string(&toolchain_path).map_err(|error| {
        validation_error(format!(
            "failed to read {}: {error}",
            toolchain_path.display()
        ))
    })?;
    check_rust_version_matches_pin(rust_version, &toolchain)
}

/// Compares a `rust-version` against the `channel` of a `rust-toolchain.toml`
/// that pins an exact `X.Y.Z` release.
fn check_rust_version_matches_pin(rust_version: &str, toolchain_toml: &str) -> TaskResult<()> {
    let table: toml::Table = toml::from_str(toolchain_toml).map_err(|error| {
        validation_error(format!("rust-toolchain.toml is not valid TOML: {error}"))
    })?;
    let channel = table
        .get("toolchain")
        .and_then(|toolchain| toolchain.get("channel"))
        .and_then(Value::as_str)
        .ok_or_else(|| validation_error("rust-toolchain.toml has no [toolchain] channel"))?;
    let mut parts = channel.split('.');
    let msrv = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(major), Some(minor), Some(patch), None)
            if [major, minor, patch]
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) =>
        {
            format!("{major}.{minor}")
        }
        _ => {
            return Err(validation_error(format!(
                "rust-toolchain.toml channel must pin an exact X.Y.Z release; found {channel:?}"
            )));
        }
    };
    ensure(
        rust_version == msrv,
        format!(
            "workspace.package.rust-version must be {msrv}, the pinned toolchain {channel}; found {rust_version:?}"
        ),
    )
}

/// Ensures required documentation files contain the branded binary, version,
/// and daemon-config snippets.
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
            relative_path: "docs/feature_matrix.md",
            required_snippets: vec![
                branding.client_bin.clone(),
                branding.daemon_bin.clone(),
                branding.daemon_config.display().to_string(),
                branding.daemon_secrets.display().to_string(),
            ],
        },
    ];

    for check in checks {
        let path = workspace.join(check.relative_path);
        let contents = read_file_with_context(&path)?;

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
                    .map(|snippet| format!("'{snippet}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_rust_version_matches_pin;
    use std::path::Path;

    fn pin(channel: &str) -> String {
        format!("[toolchain]\nchannel = \"{channel}\"\n")
    }

    #[test]
    fn msrv_is_the_major_minor_of_the_pinned_toolchain() {
        assert!(check_rust_version_matches_pin("1.100", &pin("1.100.0")).is_ok());
        assert!(check_rust_version_matches_pin("1.89", &pin("1.100.0")).is_err());
        assert!(check_rust_version_matches_pin("1.100.0", &pin("1.100.0")).is_err());
        assert!(check_rust_version_matches_pin("1.10", &pin("1.100.0")).is_err());
    }

    #[test]
    fn a_floating_or_malformed_pin_is_refused() {
        for channel in [
            "stable",
            "1.100",
            "1.100.0.1",
            "beta-2026-11-23",
            "1.x.0",
            "1..0",
        ] {
            assert!(
                check_rust_version_matches_pin("1.100", &pin(channel)).is_err(),
                "{channel} was accepted"
            );
        }
        assert!(check_rust_version_matches_pin("1.100", "[toolchain]\n").is_err());
        assert!(check_rust_version_matches_pin("1.100", "not toml =").is_err());
    }

    /// The repository's own manifest and pin agree, so a toolchain bump that
    /// forgets the MSRV (or the reverse) fails here before preflight runs.
    #[test]
    fn workspace_msrv_matches_the_repository_pin() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let manifest: toml::Table =
            toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml"))
                .expect("Cargo.toml parses");
        let rust_version = manifest["workspace"]["package"]["rust-version"]
            .as_str()
            .expect("workspace.package.rust-version");
        let toolchain =
            std::fs::read_to_string(root.join("rust-toolchain.toml")).expect("rust-toolchain.toml");
        check_rust_version_matches_pin(rust_version, &toolchain).expect("MSRV matches the pin");
    }
}
