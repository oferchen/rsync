use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use std::path::Path;

pub(super) fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
    if branding.brand.trim().is_empty() {
        return Err(TaskError::Validation(String::from(
            "workspace brand label must not be empty",
        )));
    }

    let expected_prefix = format!("{}-", branding.brand);

    if !(28..=32).contains(&branding.protocol) {
        return Err(TaskError::Validation(format!(
            "protocol version {} must be between 28 and 32",
            branding.protocol
        )));
    }

    if !branding.client_bin.starts_with(&expected_prefix) {
        return Err(TaskError::Validation(format!(
            "client binary '{}' must use '{}' prefix",
            branding.client_bin, expected_prefix
        )));
    }

    if !branding.daemon_bin.starts_with(&expected_prefix) {
        return Err(TaskError::Validation(format!(
            "daemon binary '{}' must use '{}' prefix",
            branding.daemon_bin, expected_prefix
        )));
    }

    let expected_dir_suffix = format!("{}-rsyncd", branding.brand);
    if branding
        .daemon_config_dir
        .as_path()
        .file_name()
        .and_then(|name| name.to_str())
        != Some(expected_dir_suffix.as_str())
    {
        return Err(TaskError::Validation(format!(
            "daemon_config_dir '{}' must end with '{}'",
            branding.daemon_config_dir.display(),
            expected_dir_suffix
        )));
    }

    ensure_named_file(
        branding.daemon_config.as_path(),
        &format!("{}-rsyncd.conf", branding.brand),
        "daemon_config",
    )?;
    ensure_named_file(
        branding.daemon_secrets.as_path(),
        &format!("{}-rsyncd.secrets", branding.brand),
        "daemon_secrets",
    )?;
    ensure_named_file(
        branding.legacy_daemon_config.as_path(),
        "rsyncd.conf",
        "legacy_daemon_config",
    )?;
    ensure_named_file(
        branding.legacy_daemon_secrets.as_path(),
        "rsyncd.secrets",
        "legacy_daemon_secrets",
    )?;

    if branding.legacy_client_bin != "rsync" {
        return Err(TaskError::Validation(format!(
            "legacy client binary '{}' must be 'rsync'",
            branding.legacy_client_bin
        )));
    }

    if branding.legacy_daemon_bin != "rsyncd" {
        return Err(TaskError::Validation(format!(
            "legacy daemon binary '{}' must be 'rsyncd'",
            branding.legacy_daemon_bin
        )));
    }

    if branding.legacy_daemon_config_dir.as_path().to_str() != Some("/etc") {
        return Err(TaskError::Validation(format!(
            "legacy_daemon_config_dir '{}' must be '/etc'",
            branding.legacy_daemon_config_dir.display()
        )));
    }

    ensure_absolute(&branding.daemon_config_dir, "daemon_config_dir")?;
    ensure_absolute(&branding.daemon_config, "daemon_config")?;
    ensure_absolute(&branding.daemon_secrets, "daemon_secrets")?;
    ensure_absolute(
        &branding.legacy_daemon_config_dir,
        "legacy_daemon_config_dir",
    )?;
    ensure_absolute(&branding.legacy_daemon_config, "legacy_daemon_config")?;
    ensure_absolute(&branding.legacy_daemon_secrets, "legacy_daemon_secrets")?;

    if !branding
        .rust_version
        .starts_with(&branding.upstream_version)
    {
        return Err(TaskError::Validation(format!(
            "rust_version '{}' must include upstream_version '{}' prefix",
            branding.rust_version, branding.upstream_version
        )));
    }

    if !branding.rust_version.ends_with("-rust") {
        return Err(TaskError::Validation(format!(
            "rust_version '{}' must end with '-rust' suffix",
            branding.rust_version
        )));
    }

    ensure_under_dir(
        &branding.daemon_config,
        &branding.daemon_config_dir,
        "daemon_config",
        "daemon_config_dir",
    )?;
    ensure_under_dir(
        &branding.daemon_secrets,
        &branding.daemon_config_dir,
        "daemon_secrets",
        "daemon_config_dir",
    )?;
    ensure_under_dir(
        &branding.legacy_daemon_config,
        &branding.legacy_daemon_config_dir,
        "legacy_daemon_config",
        "legacy_daemon_config_dir",
    )?;
    ensure_under_dir(
        &branding.legacy_daemon_secrets,
        &branding.legacy_daemon_config_dir,
        "legacy_daemon_secrets",
        "legacy_daemon_config_dir",
    )?;

    Ok(())
}

fn ensure_absolute(path: &Path, label: &str) -> TaskResult<()> {
    if !path.is_absolute() {
        return Err(TaskError::Validation(format!(
            "{label} '{}' must be an absolute path",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_under_dir(
    path: &Path,
    expected_dir: &Path,
    path_label: &str,
    dir_label: &str,
) -> TaskResult<()> {
    let parent = path.parent().ok_or_else(|| {
        TaskError::Validation(format!(
            "{path_label} '{}' must reside under {dir_label} '{}'",
            path.display(),
            expected_dir.display()
        ))
    })?;

    if parent != expected_dir {
        return Err(TaskError::Validation(format!(
            "{path_label} '{}' must reside under {dir_label} '{}'",
            path.display(),
            expected_dir.display()
        )));
    }

    Ok(())
}

fn ensure_named_file(path: &Path, expected: &str, label: &str) -> TaskResult<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            TaskError::Validation(format!(
                "{label} '{}' must include a file name",
                path.display()
            ))
        })?;

    if name != expected {
        return Err(TaskError::Validation(format!(
            "{label} '{}' must be named '{expected}'",
            path.display()
        )));
    }

    Ok(())
}
