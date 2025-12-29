use crate::error::{TaskError, TaskResult};
use crate::util::{read_file_with_context, resolve_workspace_path, validation_error};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use toml::Value;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct LineLimitsConfig {
    pub(super) default_max_lines: Option<usize>,
    pub(super) default_warn_lines: Option<usize>,
    overrides: HashMap<PathBuf, FileLineLimit>,
}

impl LineLimitsConfig {
    pub(super) fn override_for(&self, path: &Path) -> Option<&FileLineLimit> {
        self.overrides.get(path)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FileLineLimit {
    pub(super) max_lines: Option<usize>,
    pub(super) warn_lines: Option<usize>,
}

pub(super) fn resolve_config_path(
    workspace: &Path,
    override_path: Option<PathBuf>,
) -> TaskResult<Option<PathBuf>> {
    if let Some(path) = override_path {
        return Ok(Some(resolve_workspace_path(workspace, path)));
    }

    let default = workspace.join("tools/line_limits.toml");
    if default.exists() {
        Ok(Some(default))
    } else {
        Ok(None)
    }
}

pub(super) fn load_line_limits_config(
    workspace: &Path,
    path: &Path,
) -> TaskResult<LineLimitsConfig> {
    let contents = read_file_with_context(path)?;
    let config = parse_line_limits_config(&contents, path)?;
    validate_line_limit_overrides(workspace, path, &config)?;
    Ok(config)
}

pub(super) fn parse_line_limits_config(
    contents: &str,
    origin: &Path,
) -> TaskResult<LineLimitsConfig> {
    let value = contents.parse::<Value>().map_err(|error| {
        TaskError::Metadata(format!("failed to parse {}: {error}", origin.display()))
    })?;

    let mut config = LineLimitsConfig::default();
    if let Some(default_max) = value.get("default_max_lines") {
        config.default_max_lines = Some(parse_positive_usize_value(
            default_max,
            "default_max_lines",
            origin,
        )?);
    }

    if let Some(default_warn) = value.get("default_warn_lines") {
        config.default_warn_lines = Some(parse_positive_usize_value(
            default_warn,
            "default_warn_lines",
            origin,
        )?);
    }

    if let (Some(warn), Some(max)) = (config.default_warn_lines, config.default_max_lines)
        && warn > max
    {
        return Err(validation_error(format!(
            "default warn_lines ({warn}) cannot exceed default max_lines ({max}) in {}",
            origin.display()
        )));
    }

    if let Some(overrides) = value.get("overrides") {
        let entries = overrides.as_array().ok_or_else(|| {
            validation_error(format!(
                "'overrides' in {} must be an array",
                origin.display()
            ))
        })?;

        for (index, entry) in entries.iter().enumerate() {
            let entry_table = entry.as_table().ok_or_else(|| {
                validation_error(format!(
                    "override #{index} in {} must be a table",
                    origin.display()
                ))
            })?;

            let path_value = entry_table
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    validation_error(format!(
                        "override #{index} in {} must include a string 'path' field",
                        origin.display()
                    ))
                })?;

            let override_path = PathBuf::from(path_value);
            if override_path.as_os_str().is_empty() {
                return Err(validation_error(format!(
                    "override #{index} in {} has an empty path",
                    origin.display()
                )));
            }

            if override_path.is_absolute() {
                return Err(validation_error(format!(
                    "override path {} in {} must be relative",
                    override_path.display(),
                    origin.display()
                )));
            }

            if override_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(validation_error(format!(
                    "override path {} in {} may not contain parent directory components",
                    override_path.display(),
                    origin.display()
                )));
            }

            if config.overrides.contains_key(&override_path) {
                return Err(validation_error(format!(
                    "duplicate override for {} in {}",
                    override_path.display(),
                    origin.display()
                )));
            }

            let max_key = format!("overrides[{index}].max_lines");
            let warn_key = format!("overrides[{index}].warn_lines");

            let max_lines = entry_table
                .get("max_lines")
                .map(|value| parse_positive_usize_value(value, &max_key, origin))
                .transpose()?;
            let warn_lines = entry_table
                .get("warn_lines")
                .map(|value| parse_positive_usize_value(value, &warn_key, origin))
                .transpose()?;

            if let (Some(warn), Some(max)) = (warn_lines, max_lines)
                && warn > max
            {
                return Err(validation_error(format!(
                    "override for {} in {} has warn_lines ({warn}) exceeding max_lines ({max})",
                    override_path.display(),
                    origin.display()
                )));
            }

            config.overrides.insert(
                override_path,
                FileLineLimit {
                    max_lines,
                    warn_lines,
                },
            );
        }
    }

    Ok(config)
}

fn validate_line_limit_overrides(
    workspace: &Path,
    origin: &Path,
    config: &LineLimitsConfig,
) -> TaskResult<()> {
    for relative in config.overrides.keys() {
        let candidate = workspace.join(relative);
        if !candidate.exists() {
            return Err(validation_error(format!(
                "override path {} in {} does not exist",
                relative.display(),
                origin.display()
            )));
        }

        if !candidate.is_file() {
            return Err(validation_error(format!(
                "override path {} in {} is not a regular file",
                relative.display(),
                origin.display()
            )));
        }
    }

    Ok(())
}

fn parse_positive_usize_value(value: &Value, field: &str, origin: &Path) -> TaskResult<usize> {
    let integer = value.as_integer().ok_or_else(|| {
        validation_error(format!(
            "{field} in {} must be an integer",
            origin.display()
        ))
    })?;

    if integer <= 0 {
        return Err(validation_error(format!(
            "{field} in {} must be a positive integer",
            origin.display()
        )));
    }

    usize::try_from(integer).map_err(|_| {
        validation_error(format!(
            "{field} in {} exceeds supported range",
            origin.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_config(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create config directory");
        }
        fs::write(path, contents).expect("write config file");
    }

    fn create_workspace() -> tempfile::TempDir {
        let dir = tempdir().expect("create workspace");
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).expect("create src directory");
        fs::write(src_dir.join("lib.rs"), "fn main() {}\n").expect("write source file");
        dir
    }

    #[test]
    fn resolve_config_path_prefers_override_and_detects_default() {
        let workspace = create_workspace();
        let override_path = workspace.path().join("custom.toml");
        write_config(&override_path, "{}");

        let resolved = resolve_config_path(workspace.path(), Some(PathBuf::from("custom.toml")))
            .expect("resolve succeeds")
            .expect("path present");
        assert_eq!(resolved, override_path);

        let default_path = workspace.path().join("tools/line_limits.toml");
        write_config(&default_path, "{}");
        let resolved_default = resolve_config_path(workspace.path(), None)
            .expect("resolve succeeds")
            .expect("default present");
        assert_eq!(resolved_default, default_path);

        fs::remove_file(&default_path).expect("remove default");
        let missing = resolve_config_path(workspace.path(), None).expect("resolve succeeds");
        assert!(missing.is_none());
    }

    #[test]
    fn load_line_limits_config_accepts_existing_overrides() {
        let workspace = create_workspace();
        let config_path = workspace.path().join("tools/line_limits.toml");
        write_config(
            &config_path,
            r#"
default_max_lines = 600

[[overrides]]
path = "src/lib.rs"
warn_lines = 10
"#,
        );

        let config = load_line_limits_config(workspace.path(), &config_path).expect("config loads");
        assert_eq!(config.default_max_lines, Some(600));
        let override_entry = config
            .override_for(Path::new("src/lib.rs"))
            .expect("override present");
        assert_eq!(override_entry.warn_lines, Some(10));
    }

    #[test]
    fn load_line_limits_config_rejects_missing_overrides() {
        let workspace = create_workspace();
        let config_path = workspace.path().join("line_limits.toml");
        write_config(
            &config_path,
            r#"
[[overrides]]
path = "src/missing.rs"
max_lines = 5
"#,
        );

        let error = load_line_limits_config(workspace.path(), &config_path).unwrap_err();
        assert!(matches!(error, TaskError::Validation(message) if message.contains("missing.rs")));
    }
}
