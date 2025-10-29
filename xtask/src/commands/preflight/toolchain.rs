use crate::error::TaskResult;
use crate::util::ensure;
use crate::util::validation_error;
use toml::Value;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_expected_rust_version() {
        let manifest: Value = r#"
            [workspace.package]
            rust-version = "1.87"
        "#
        .parse()
        .unwrap();

        assert!(validate_workspace_package_rust_version(&manifest).is_ok());
    }
}
