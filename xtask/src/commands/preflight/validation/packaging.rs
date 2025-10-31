use crate::error::{TaskError, TaskResult};
use crate::util::{ensure, validation_error};
use crate::workspace::WorkspaceBranding;
use std::fs;
use std::path::{Path, PathBuf};
use toml::{Value, value::Table as TomlTable};

pub(crate) fn validate_packaging_assets(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
    let packaging_root = workspace.join("packaging").join("etc").join("oc-rsyncd");
    let config_name = file_name(branding.daemon_config.as_path(), "daemon_config")?;
    let secrets_name = file_name(branding.daemon_secrets.as_path(), "daemon_secrets")?;

    for (name, label) in [
        (config_name, "daemon_config"),
        (secrets_name, "daemon_secrets"),
    ] {
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

    validate_bin_manifest_packaging(workspace, branding)?;
    validate_systemd_unit(workspace, branding)?;
    validate_default_env(workspace)?;

    Ok(())
}

pub(super) fn file_name(path: &Path, label: &str) -> TaskResult<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| validation_error(format!("{label} must include a file name")))?;
    Ok(PathBuf::from(name))
}

fn validate_bin_manifest_packaging(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
    let manifest_path = workspace.join("Cargo.toml");
    let manifest_text = fs::read_to_string(&manifest_path).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!("failed to read {}: {error}", manifest_path.display()),
        ))
    })?;

    let manifest_value: Value = manifest_text.parse().map_err(|error| {
        TaskError::Metadata(format!(
            "failed to parse {}: {error}",
            manifest_path.display()
        ))
    })?;

    let daemon_config = branding.daemon_config.display().to_string();
    let daemon_secrets = branding.daemon_secrets.display().to_string();

    let package = manifest_value
        .get("package")
        .and_then(Value::as_table)
        .ok_or_else(|| {
            validation_error(format!(
                "{} missing [package] table",
                manifest_path.display()
            ))
        })?;
    let metadata = package
        .get("metadata")
        .and_then(Value::as_table)
        .ok_or_else(|| {
            validation_error(format!(
                "{} missing [package.metadata] table",
                manifest_path.display()
            ))
        })?;

    let deb = metadata
        .get("deb")
        .and_then(Value::as_table)
        .ok_or_else(|| {
            validation_error(format!(
                "{} missing [package.metadata.deb] table",
                manifest_path.display()
            ))
        })?;
    ensure(
        deb_assets_include(deb, daemon_config.as_str()),
        format!(
            "{} package.metadata.deb.assets must install {}",
            manifest_path.display(),
            branding.daemon_config.display()
        ),
    )?;
    ensure(
        deb_assets_include(deb, daemon_secrets.as_str()),
        format!(
            "{} package.metadata.deb.assets must install {}",
            manifest_path.display(),
            branding.daemon_secrets.display()
        ),
    )?;

    ensure(
        deb_conf_files_include(deb, daemon_config.as_str()),
        format!(
            "{} package.metadata.deb.conf-files must reference {}",
            manifest_path.display(),
            branding.daemon_config.display()
        ),
    )?;
    ensure(
        deb_conf_files_include(deb, daemon_secrets.as_str()),
        format!(
            "{} package.metadata.deb.conf-files must reference {}",
            manifest_path.display(),
            branding.daemon_secrets.display()
        ),
    )?;

    let rpm = metadata
        .get("rpm")
        .and_then(Value::as_table)
        .ok_or_else(|| {
            validation_error(format!(
                "{} missing [package.metadata.rpm] table",
                manifest_path.display()
            ))
        })?;
    ensure(
        rpm_assets_include(rpm, daemon_config.as_str()),
        format!(
            "{} package.metadata.rpm.assets must install {} with config=true",
            manifest_path.display(),
            branding.daemon_config.display()
        ),
    )?;
    ensure(
        rpm_assets_include(rpm, daemon_secrets.as_str()),
        format!(
            "{} package.metadata.rpm.assets must install {} with config=true",
            manifest_path.display(),
            branding.daemon_secrets.display()
        ),
    )?;

    Ok(())
}

fn validate_systemd_unit(workspace: &Path, branding: &WorkspaceBranding) -> TaskResult<()> {
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

    let unit_daemon_config = branding.daemon_config.display().to_string();
    let unit_daemon_secrets = branding.daemon_secrets.display().to_string();
    let unit_snippets = [
        branding.daemon_bin.as_str(),
        unit_daemon_config.as_str(),
        unit_daemon_secrets.as_str(),
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

    Ok(())
}

fn validate_default_env(workspace: &Path) -> TaskResult<()> {
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

fn deb_assets_include(table: &TomlTable, destination: &str) -> bool {
    table
        .get("assets")
        .and_then(Value::as_array)
        .is_some_and(|assets| {
            assets.iter().any(|entry| match entry {
                Value::Array(items) if items.len() >= 2 => {
                    items.get(1).and_then(Value::as_str) == Some(destination)
                }
                _ => false,
            })
        })
}

fn deb_conf_files_include(table: &TomlTable, absolute_path: &str) -> bool {
    let Some(relative) = absolute_path.strip_prefix('/') else {
        return false;
    };

    table
        .get("conf-files")
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(|entry| entry.as_str() == Some(relative)))
}

fn rpm_assets_include(table: &TomlTable, destination: &str) -> bool {
    table
        .get("assets")
        .and_then(Value::as_array)
        .is_some_and(|assets| {
            assets.iter().any(|entry| match entry {
                Value::Table(map) => {
                    map.get("dest").and_then(Value::as_str) == Some(destination)
                        && map.get("config").and_then(Value::as_bool).unwrap_or(false)
                }
                _ => false,
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deb_asset_helpers_match_expected_entries() {
        let manifest = r#"
            [package.metadata.deb]
            assets = [["source", "/etc/oc-rsyncd/oc-rsyncd.conf", "644"], ["source2", "/etc/oc-rsyncd/oc-rsyncd.secrets", "600"]]
            conf-files = ["etc/oc-rsyncd/oc-rsyncd.conf", "etc/oc-rsyncd/oc-rsyncd.secrets"]
        "#;
        let value: Value = manifest.parse().expect("parse succeeds");
        let deb = value
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("metadata"))
            .and_then(Value::as_table)
            .and_then(|metadata| metadata.get("deb"))
            .and_then(Value::as_table)
            .expect("deb table present");

        assert!(deb_assets_include(deb, "/etc/oc-rsyncd/oc-rsyncd.conf"));
        assert!(deb_assets_include(deb, "/etc/oc-rsyncd/oc-rsyncd.secrets"));
        assert!(deb_conf_files_include(deb, "/etc/oc-rsyncd/oc-rsyncd.conf"));
        assert!(deb_conf_files_include(
            deb,
            "/etc/oc-rsyncd/oc-rsyncd.secrets"
        ));
    }

    #[test]
    fn rpm_asset_helper_requires_config_flag() {
        let manifest = r#"
            [package.metadata.rpm]
            assets = [
                { path = "src", dest = "/etc/oc-rsyncd/oc-rsyncd.conf", mode = "0644", config = true },
                { path = "src2", dest = "/etc/oc-rsyncd/oc-rsyncd.secrets", mode = "0600", config = true }
            ]
        "#;
        let value: Value = manifest.parse().expect("parse succeeds");
        let rpm = value
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("metadata"))
            .and_then(Value::as_table)
            .and_then(|metadata| metadata.get("rpm"))
            .and_then(Value::as_table)
            .expect("rpm table present");

        assert!(rpm_assets_include(rpm, "/etc/oc-rsyncd/oc-rsyncd.conf"));
        assert!(rpm_assets_include(rpm, "/etc/oc-rsyncd/oc-rsyncd.secrets"));

        let manifest_missing_flag = r#"
            [package.metadata.rpm]
            assets = [
                { path = "src", dest = "/etc/oc-rsyncd/oc-rsyncd.conf", mode = "0644" }
            ]
        "#;
        let value: Value = manifest_missing_flag.parse().expect("parse succeeds");
        let rpm = value
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("metadata"))
            .and_then(Value::as_table)
            .and_then(|metadata| metadata.get("rpm"))
            .and_then(Value::as_table)
            .expect("rpm table present");
        assert!(!rpm_assets_include(rpm, "/etc/oc-rsyncd/oc-rsyncd.conf"));
    }
}
