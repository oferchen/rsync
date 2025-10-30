use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use std::path::Path;

pub fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::parse_workspace_branding;
    use std::path::{Path, PathBuf};

    fn sample_branding() -> WorkspaceBranding {
        WorkspaceBranding {
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
            source: String::from("https://example.invalid/rsync"),
        }
    }

    fn manifest_branding() -> WorkspaceBranding {
        let manifest = include_str!("../../../../Cargo.toml");
        parse_workspace_branding(manifest).expect("manifest parses")
    }

    #[test]
    fn validate_branding_accepts_manifest_configuration() {
        let branding = manifest_branding();
        validate_branding(&branding).expect("validation succeeds");
    }

    #[test]
    fn validate_branding_accepts_prefixed_binaries() {
        let branding = sample_branding();
        validate_branding(&branding).expect("validation succeeds");
    }

    #[test]
    fn validate_branding_rejects_protocol_outside_supported_range() {
        let mut branding = sample_branding();
        branding.protocol = 27;
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("protocol version")
        ));
    }

    #[test]
    fn validate_branding_rejects_missing_client_prefix() {
        let mut branding = sample_branding();
        branding.client_bin = String::from("rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("client binary")
        ));
    }

    #[test]
    fn validate_branding_rejects_missing_daemon_prefix() {
        let mut branding = sample_branding();
        branding.daemon_bin = String::from("rsyncd");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("daemon binary")
        ));
    }

    #[test]
    fn validate_branding_rejects_empty_brand() {
        let mut branding = sample_branding();
        branding.brand.clear();
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("must not be empty")
        ));
    }

    #[test]
    fn validate_branding_rejects_non_absolute_paths() {
        let mut branding = sample_branding();
        branding.daemon_config_dir = Path::new("relative").into();
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("daemon_config_dir")
        ));
    }

    #[test]
    fn validate_branding_rejects_empty_legacy_names() {
        let mut branding = sample_branding();
        branding.legacy_client_bin.clear();
        let client_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            client_error,
            TaskError::Validation(message) if message.contains("legacy client binary")
        ));

        let mut branding = sample_branding();
        branding.legacy_daemon_bin.clear();
        let daemon_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            daemon_error,
            TaskError::Validation(message) if message.contains("legacy daemon binary")
        ));
    }

    #[test]
    fn validate_branding_rejects_incorrect_legacy_names() {
        let mut branding = sample_branding();
        branding.legacy_client_bin = String::from("legacy-rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("legacy client binary")
        ));

        let mut branding = sample_branding();
        branding.legacy_daemon_bin = String::from("legacy-rsyncd");
        let daemon_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            daemon_error,
            TaskError::Validation(message) if message.contains("legacy daemon binary")
        ));
    }

    #[test]
    fn validate_branding_requires_legacy_directory() {
        let mut branding = sample_branding();
        branding.legacy_daemon_config_dir = PathBuf::from("/opt");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message)
                if message.contains("legacy_daemon_config_dir")
        ));
    }

    #[test]
    fn validate_branding_requires_rust_version_prefix_and_suffix() {
        let mut branding = sample_branding();
        branding.rust_version = String::from("4.0.0-rust");
        let prefix_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            prefix_error,
            TaskError::Validation(message)
                if message.contains("must include upstream_version")
        ));

        let mut branding = sample_branding();
        branding.rust_version = String::from("3.4.1-custom");
        let suffix_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            suffix_error,
            TaskError::Validation(message) if message.contains("must end with")
        ));
    }

    #[test]
    fn validate_branding_requires_paths_to_match_directories() {
        let mut branding = sample_branding();
        branding.daemon_config = PathBuf::from("/etc/oc-rsyncd.conf");
        let daemon_config_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            daemon_config_error,
            TaskError::Validation(message) if message.contains("daemon_config")
        ));

        let mut branding = sample_branding();
        branding.legacy_daemon_secrets = PathBuf::from("/var/lib/rsyncd.secrets");
        let legacy_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            legacy_error,
            TaskError::Validation(message) if message.contains("legacy_daemon_secrets")
        ));
    }

    #[test]
    fn validate_branding_requires_daemon_directory_suffix() {
        let mut branding = sample_branding();
        branding.daemon_config_dir = PathBuf::from("/etc/oc-rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("daemon_config_dir")
        ));
    }

    #[test]
    fn validate_branding_requires_daemon_file_names() {
        let mut branding = sample_branding();
        branding.daemon_config = PathBuf::from("/etc/oc-rsyncd/custom.conf");
        let config_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            config_error,
            TaskError::Validation(message)
                if message.contains("daemon_config '/etc/oc-rsyncd/custom.conf' must be named")
        ));

        let mut branding = sample_branding();
        branding.daemon_secrets = PathBuf::from("/etc/oc-rsyncd/rsyncd.secret");
        let secrets_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            secrets_error,
            TaskError::Validation(message) if message.contains("daemon_secrets")
        ));
    }

    #[test]
    fn validate_branding_requires_legacy_file_names() {
        let mut branding = sample_branding();
        branding.legacy_daemon_config = PathBuf::from("/etc/rsync.conf");
        let config_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            config_error,
            TaskError::Validation(message) if message.contains("legacy_daemon_config")
        ));

        let mut branding = sample_branding();
        branding.legacy_daemon_secrets = PathBuf::from("/etc/rsync.secret");
        let secrets_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            secrets_error,
            TaskError::Validation(message) if message.contains("legacy_daemon_secrets")
        ));
    }
}
