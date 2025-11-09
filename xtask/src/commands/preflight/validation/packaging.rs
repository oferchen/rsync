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
    let brand = branding.brand.trim();
    if brand.is_empty() {
        return Err(validation_error("workspace brand label must not be empty"));
    }

    let config_dir_name = branding
        .daemon_config_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            validation_error(format!(
                "daemon_config_dir '{}' must include a terminal component",
                branding.daemon_config_dir.display()
            ))
        })?;

    let packaging_root = workspace
        .join("packaging")
        .join("etc")
        .join(config_dir_name);
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
    validate_default_env(workspace, branding)?;

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
    let brand = branding.brand.trim();
    ensure(!brand.is_empty(), "workspace brand label must not be empty")?;

    let unit_label = format!("{brand}-rsyncd");
    let unit_filename = format!("{unit_label}.service");
    let systemd_unit = workspace
        .join("packaging")
        .join("systemd")
        .join(&unit_filename);
    let unit_contents = fs::read_to_string(&systemd_unit).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!("failed to read {}: {}", systemd_unit.display(), error),
        ))
    })?;

    let unit_daemon_config = branding.daemon_config.display().to_string();
    let unit_daemon_secrets = branding.daemon_secrets.display().to_string();
    let expected_description =
        format!("Description={unit_label} daemon providing rsync protocol services");
    let unit_snippets = [
        branding.daemon_bin.as_str(),
        unit_daemon_config.as_str(),
        unit_daemon_secrets.as_str(),
        branding.source.as_str(),
        "OC_RSYNC_CONFIG",
        "OC_RSYNC_SECRETS",
        "RSYNCD_CONFIG",
        "RSYNCD_SECRETS",
    ];

    for snippet in unit_snippets {
        ensure(
            unit_contents.contains(snippet),
            format!(
                "systemd unit {} missing required snippet '{}': update packaging/systemd/{}",
                systemd_unit.display(),
                snippet,
                unit_filename
            ),
        )?;
    }

    ensure(
        unit_contents
            .lines()
            .any(|line| line.trim() == expected_description),
        format!(
            "systemd unit {} missing description '{}': update packaging/systemd/{}",
            systemd_unit.display(),
            expected_description,
            unit_filename,
        ),
    )?;

    let expected_documentation_line = format!("Documentation={}", branding.source);
    ensure(
        unit_contents
            .lines()
            .any(|line| line.trim() == expected_documentation_line),
        format!(
            "systemd unit {} must declare '{}' to keep Documentation aligned with workspace branding",
            systemd_unit.display(),
            expected_documentation_line
        ),
    )?;

    ensure(
        !unit_contents.contains("Alias=rsyncd.service"),
        format!(
            "systemd unit {} must not define the legacy alias 'rsyncd.service' to allow co-installation with upstream packages",
            systemd_unit.display()
        ),
    )?;

    Ok(())
}

fn validate_default_env(workspace: &Path, branding: &WorkspaceBranding) -> TaskResult<()> {
    let brand = branding.brand.trim();
    ensure(!brand.is_empty(), "workspace brand label must not be empty")?;
    let env_basename = format!("{brand}-rsyncd");

    let env_file = workspace
        .join("packaging")
        .join("default")
        .join(&env_basename);
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
                "environment defaults {} missing '{}': update packaging/default/{}",
                env_file.display(),
                snippet,
                env_basename
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
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn sample_branding() -> WorkspaceBranding {
        WorkspaceBranding {
            brand: String::from("oc"),
            upstream_version: String::from("3.4.1"),
            rust_version: String::from("3.4.1-rust"),
            protocol: 32,
            client_bin: String::from("oc-rsync"),
            daemon_bin: String::from("oc-rsync"),
            legacy_client_bin: String::from("rsync"),
            legacy_daemon_bin: String::from("rsyncd"),
            daemon_config_dir: PathBuf::from("/etc/oc-rsyncd"),
            daemon_config: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
            daemon_secrets: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
            legacy_daemon_config_dir: PathBuf::from("/etc"),
            legacy_daemon_config: PathBuf::from("/etc/rsyncd.conf"),
            legacy_daemon_secrets: PathBuf::from("/etc/rsyncd.secrets"),
            source: String::from("https://example.invalid/oc-rsync"),
            cross_compile: BTreeMap::new(),
            cross_compile_matrix: BTreeMap::new(),
        }
    }

    fn write_unit_file(root: &Path, branding: &WorkspaceBranding, documentation: &str) {
        let systemd_dir = root.join("packaging").join("systemd");
        fs::create_dir_all(&systemd_dir).expect("create systemd directory");
        let unit_label = format!("{}-rsyncd", branding.brand.trim());
        let path = systemd_dir.join(format!("{unit_label}.service"));
        let config = branding.daemon_config.display().to_string();
        let secrets = branding.daemon_secrets.display().to_string();
        let contents = format!(
            "[Unit]\n\
             Description={unit_label} daemon providing rsync protocol services\n\
             Documentation={documentation}\n\
             [Service]\n\
             Environment=\"OC_RSYNC_CONFIG={config}\"\n\
             Environment=\"OC_RSYNC_SECRETS={secrets}\"\n\
             Environment=\"RSYNCD_CONFIG={config}\"\n\
             Environment=\"RSYNCD_SECRETS={secrets}\"\n\
             ExecStart=/usr/bin/{binary} --daemon --config ${{RSYNCD_CONFIG}} $RSYNCD_ARGS\n\
             [Install]\n\
             WantedBy=multi-user.target\n",
            documentation = documentation,
            config = config,
            secrets = secrets,
            binary = branding.daemon_bin,
        );
        fs::write(path, contents).expect("write systemd unit");
    }

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

    #[test]
    fn validate_systemd_unit_accepts_workspace_source_url() {
        let branding = sample_branding();
        let temp = tempdir().expect("tempdir");
        write_unit_file(temp.path(), &branding, &branding.source);

        validate_systemd_unit(temp.path(), &branding)
            .expect("systemd unit should satisfy branding checks");
    }

    #[test]
    fn validate_systemd_unit_rejects_mismatched_documentation_url() {
        let branding = sample_branding();
        let temp = tempdir().expect("tempdir");
        write_unit_file(temp.path(), &branding, "https://example.invalid/other");

        let error = validate_systemd_unit(temp.path(), &branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains(&branding.source)
        ));
    }
}
