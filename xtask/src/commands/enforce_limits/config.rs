use crate::error::{TaskError, TaskResult};
use crate::util::validation_error;
use std::collections::HashMap;
use std::fs;
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
        let absolute = if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        };
        return Ok(Some(absolute));
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
    let contents = fs::read_to_string(path).map_err(|error| {
        TaskError::Io(std::io::Error::new(
            error.kind(),
            format!("failed to read {}: {error}", path.display()),
        ))
    })?;
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

    if let (Some(warn), Some(max)) = (config.default_warn_lines, config.default_max_lines) {
        if warn > max {
            return Err(validation_error(format!(
                "default warn_lines ({warn}) cannot exceed default max_lines ({max}) in {}",
                origin.display()
            )));
        }
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

            if let (Some(warn), Some(max)) = (warn_lines, max_lines) {
                if warn > max {
                    return Err(validation_error(format!(
                        "override for {} in {} has warn_lines ({warn}) exceeding max_lines ({max})",
                        override_path.display(),
                        origin.display()
                    )));
                }
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
