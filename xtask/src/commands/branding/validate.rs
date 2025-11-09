use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use std::path::Path;

pub fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
    if branding.brand.trim().is_empty() {
        return Err(TaskError::Validation(String::from(
            "workspace brand label must not be empty",
        )));
    }

    if !(28..=32).contains(&branding.protocol) {
        return Err(TaskError::Validation(format!(
            "protocol version {} must be between 28 and 32",
            branding.protocol
        )));
    }

    ensure_binary_name(&branding.client_bin, "client_bin")?;
    ensure_binary_name(&branding.daemon_bin, "daemon_bin")?;

    if branding.daemon_bin != branding.client_bin {
        return Err(TaskError::Validation(format!(
            "daemon binary '{}' must match client binary '{}' so a single executable handles both roles",
            branding.daemon_bin, branding.client_bin
        )));
    }

    ensure_has_file_name(branding.daemon_config.as_path(), "daemon_config")?;
    ensure_has_file_name(branding.daemon_secrets.as_path(), "daemon_secrets")?;
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
    if !path.starts_with(expected_dir) {
        return Err(TaskError::Validation(format!(
            "{path_label} '{}' must reside under {dir_label} '{}'",
            path.display(),
            expected_dir.display()
        )));
    }

    if path == expected_dir {
        return Err(TaskError::Validation(format!(
            "{path_label} '{}' must not be identical to {dir_label} '{}'",
            path.display(),
            expected_dir.display()
        )));
    }

    Ok(())
}

fn ensure_has_file_name(path: &Path, label: &str) -> TaskResult<()> {
    if path.file_name().is_none() {
        return Err(TaskError::Validation(format!(
            "{label} '{path}' must include a file name",
            path = path.display()
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
                "{label} '{path}' must include a file name",
                path = path.display()
            ))
        })?;

    if name != expected {
        return Err(TaskError::Validation(format!(
            "{label} '{path}' must be named '{expected}'",
            path = path.display()
        )));
    }

    Ok(())
}

fn ensure_binary_name(name: &str, label: &str) -> TaskResult<()> {
    if name.trim().is_empty() {
        return Err(TaskError::Validation(format!("{label} must not be empty")));
    }

    if name.chars().any(char::is_whitespace) {
        return Err(TaskError::Validation(format!(
            "{label} '{name}' must not contain whitespace"
        )));
    }

    if name.chars().any(std::path::is_separator) {
        return Err(TaskError::Validation(format!(
            "{label} '{name}' must not include path separators"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use crate::workspace::parse_workspace_branding;
    use std::path::{Path, PathBuf};

    fn sample_branding() -> WorkspaceBranding {
        test_support::workspace_branding_snapshot()
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
    fn validate_branding_rejects_client_whitespace() {
        let mut branding = sample_branding();
        branding.client_bin = String::from("oc rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message)
                if message.contains("client_bin") && message.contains("whitespace")
        ));
    }

    #[test]
    fn validate_branding_rejects_daemon_with_separator() {
        let mut branding = sample_branding();
        branding.daemon_bin = String::from("bin/oc-rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message)
                if message.contains("daemon_bin") && message.contains("path separators")
        ));
    }

    #[test]
    fn validate_branding_rejects_distinct_daemon_binary() {
        let mut branding = sample_branding();
        branding.daemon_bin = String::from("oc-rsyncd");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("must match client binary")
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
    fn validate_branding_rejects_relative_daemon_directory() {
        let mut branding = sample_branding();
        branding.daemon_config_dir = PathBuf::from("etc/oc-rsyncd");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message)
                if message.contains("daemon_config_dir") && message.contains("absolute path")
        ));
    }

    #[test]
    fn validate_branding_requires_daemon_file_names() {
        let mut branding = sample_branding();
        branding.daemon_config = PathBuf::from("/etc/oc-rsyncd");
        let config_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            config_error,
            TaskError::Validation(message)
                if message.contains("daemon_config") && message.contains("must not be identical")
        ));

        let mut branding = sample_branding();
        branding.daemon_secrets = PathBuf::from("/etc/oc-rsyncd");
        let secrets_error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            secrets_error,
            TaskError::Validation(message)
                if message.contains("daemon_secrets") && message.contains("must not be identical")
        ));
    }

    #[test]
    fn validate_branding_rejects_binary_with_path_separator() {
        let mut branding = sample_branding();
        branding.client_bin = String::from("bin/oc-rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message)
                if message.contains("client_bin") && message.contains("path separators")
        ));
    }

    #[test]
    fn validate_branding_rejects_binary_with_whitespace() {
        let mut branding = sample_branding();
        branding.daemon_bin = String::from("oc rsync");
        let error = validate_branding(&branding).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message)
                if message.contains("daemon_bin") && message.contains("whitespace")
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
