use crate::error::TaskResult;
use crate::util::ensure;
use crate::workspace::WorkspaceBranding;
use std::path::Path;

pub(super) fn validate_branding(branding: &WorkspaceBranding) -> TaskResult<()> {
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
