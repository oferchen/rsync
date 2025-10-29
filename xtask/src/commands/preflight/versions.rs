use crate::error::TaskResult;
use crate::util::{ensure, validation_error};
use crate::workspace::WorkspaceBranding;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

pub(super) fn validate_package_versions(
    metadata: &JsonValue,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_expected_package_versions() {
        let metadata: JsonValue = serde_json::json!({
            "packages": [
                { "name": "oc-rsync-bin", "version": "3.4.1-rust" },
                { "name": "oc-rsyncd-bin", "version": "3.4.1-rust" }
            ]
        });
        let branding = WorkspaceBranding {
            brand: "oc".into(),
            upstream_version: "3.4.1".into(),
            rust_version: "3.4.1-rust".into(),
            protocol: 32,
            client_bin: "oc-rsync".into(),
            daemon_bin: "oc-rsyncd".into(),
            legacy_client_bin: "rsync".into(),
            legacy_daemon_bin: "rsyncd".into(),
            daemon_config_dir: "/etc/oc-rsyncd".into(),
            daemon_config: "/etc/oc-rsyncd/oc-rsyncd.conf".into(),
            daemon_secrets: "/etc/oc-rsyncd/oc-rsyncd.secrets".into(),
            legacy_daemon_config_dir: "/etc".into(),
            legacy_daemon_config: "/etc/rsyncd.conf".into(),
            legacy_daemon_secrets: "/etc/rsyncd.secrets".into(),
            source: "https://github.com/oferchen/rsync".into(),
        };

        assert!(validate_package_versions(&metadata, &branding).is_ok());
    }
}
