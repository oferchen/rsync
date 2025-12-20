use crate::error::{TaskError, TaskResult};
use crate::util::{count_file_lines, is_help_flag, read_limit_env_var, validation_error};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

mod config;

use config::{load_line_limits_config, resolve_config_path};

/// Options accepted by the `enforce-limits` command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnforceLimitsOptions {
    /// Maximum allowed lines for Rust files.
    pub max_lines: Option<usize>,
    /// Warning threshold for Rust files.
    pub warn_lines: Option<usize>,
    /// Optional override configuration path.
    pub config_path: Option<PathBuf>,
}

/// Parses CLI arguments for the `enforce-limits` command.
pub fn parse_args<I>(args: I) -> TaskResult<EnforceLimitsOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut options = EnforceLimitsOptions::default();

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        match arg.to_string_lossy().as_ref() {
            "--max-lines" => {
                let value = args.next().ok_or_else(|| {
                    TaskError::Usage(String::from(
                        "--max-lines requires a positive integer value",
                    ))
                })?;
                let parsed = parse_positive_usize_arg(&value, "--max-lines")?;
                options.max_lines = Some(parsed);
            }
            "--warn-lines" => {
                let value = args.next().ok_or_else(|| {
                    TaskError::Usage(String::from(
                        "--warn-lines requires a positive integer value",
                    ))
                })?;
                let parsed = parse_positive_usize_arg(&value, "--warn-lines")?;
                options.warn_lines = Some(parsed);
            }
            "--config" => {
                let value = args.next().ok_or_else(|| {
                    TaskError::Usage(String::from("--config requires a path argument"))
                })?;
                if value.is_empty() {
                    return Err(TaskError::Usage(String::from(
                        "--config requires a non-empty path argument",
                    )));
                }
                options.config_path = Some(PathBuf::from(value));
            }
            other => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{other}' for enforce-limits command"
                )));
            }
        }
    }

    if let (Some(warn), Some(max)) = (options.warn_lines, options.max_lines)
        && warn > max
    {
        return Err(TaskError::Usage(format!(
            "warn line limit ({warn}) cannot exceed maximum line limit ({max})"
        )));
    }

    Ok(options)
}

/// Executes the `enforce-limits` command.
pub fn execute(workspace: &Path, options: EnforceLimitsOptions) -> TaskResult<()> {
    const DEFAULT_MAX_LINES: usize = 600;
    const DEFAULT_WARN_LINES: usize = 400;

    let EnforceLimitsOptions {
        max_lines: cli_max,
        warn_lines: cli_warn,
        config_path,
    } = options;

    let env_max = read_limit_env_var("MAX_RUST_LINES")?;
    let env_warn = read_limit_env_var("WARN_RUST_LINES")?;

    let config_path = resolve_config_path(workspace, config_path)?;
    let config = if let Some(path) = config_path {
        Some(load_line_limits_config(workspace, &path)?)
    } else {
        None
    };

    let max_lines = cli_max
        .or(env_max)
        .or_else(|| config.as_ref().and_then(|cfg| cfg.default_max_lines))
        .unwrap_or(DEFAULT_MAX_LINES);
    let warn_lines = cli_warn
        .or(env_warn)
        .or_else(|| config.as_ref().and_then(|cfg| cfg.default_warn_lines))
        .unwrap_or(DEFAULT_WARN_LINES);

    if warn_lines > max_lines {
        return Err(validation_error(format!(
            "warn line limit ({warn_lines}) cannot exceed maximum line limit ({max_lines})"
        )));
    }

    let rust_files = collect_rust_sources(workspace)?;
    if rust_files.is_empty() {
        eprintln!("No Rust sources found.");
        return Ok(());
    }

    let mut failure_detected = false;
    let mut warned = false;

    for path in rust_files {
        let mut file_max = max_lines;
        let mut file_warn = warn_lines;

        if let Some(config) = &config {
            let relative = path
                .strip_prefix(workspace)
                .map_err(|_| {
                    validation_error(format!(
                        "failed to compute path relative to workspace for {}",
                        path.display()
                    ))
                })?
                .to_path_buf();

            if let Some(override_limits) = config.override_for(&relative) {
                if let Some(max_override) = override_limits.max_lines {
                    file_max = max_override;
                }
                if let Some(warn_override) = override_limits.warn_lines {
                    file_warn = warn_override;
                }
            }

            if file_warn > file_max {
                return Err(validation_error(format!(
                    "override for {} sets warn_lines ({}) above max_lines ({})",
                    relative.display(),
                    file_warn,
                    file_max
                )));
            }
        }

        let line_count = count_file_lines(&path)?;
        if line_count > file_max {
            eprintln!(
                "::error file={}::Rust source has {} lines (limit {})",
                path.display(),
                line_count,
                file_max
            );
            failure_detected = true;
            continue;
        }

        if line_count > file_warn {
            eprintln!(
                "::warning file={}::Rust source has {} lines (target {})",
                path.display(),
                line_count,
                file_warn
            );
            warned = true;
        }
    }

    if failure_detected {
        return Err(validation_error(
            "Rust source files exceed the enforced maximum line count.",
        ));
    }

    if warned {
        eprintln!("Rust source files exceed target length but remain under the enforced limit.");
    }

    Ok(())
}

fn parse_positive_usize_arg(value: &OsString, flag: &str) -> TaskResult<usize> {
    let text = value.to_str().ok_or_else(|| {
        TaskError::Usage(format!("{flag} requires a UTF-8 positive integer value"))
    })?;

    let parsed = text.parse::<usize>().map_err(|_| {
        TaskError::Usage(format!(
            "{flag} requires a positive integer value, found '{text}'"
        ))
    })?;

    if parsed == 0 {
        return Err(TaskError::Usage(format!(
            "{flag} requires a positive integer value, found '{text}'"
        )));
    }

    Ok(parsed)
}

fn collect_rust_sources(root: &Path) -> TaskResult<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                if should_skip_directory(&path) {
                    continue;
                }
                stack.push(path);
                continue;
            }

            if metadata.is_file()
                && let Some(ext) = path.extension()
                && ext.eq_ignore_ascii_case("rs")
            {
                files.push(path);
            }
        }
    }

    files.sort();
    Ok(files)
}

fn should_skip_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("target") | Some(".git")
    )
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask enforce-limits [OPTIONS]\n\nOptions:\n  --max-lines N    Fail when a Rust source exceeds N lines\n  --warn-lines N   Warn when a Rust source exceeds N lines\n  --config PATH    Override the line limit configuration path\n  -h, --help       Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::config::parse_line_limits_config;
    use super::*;
    use crate::util::test_env::EnvGuard;
    use std::io::Write;
    use tempfile::tempdir;

    fn cleared_limits_env() -> EnvGuard {
        let mut guard = EnvGuard::new();
        guard.remove("MAX_RUST_LINES");
        guard.remove("WARN_RUST_LINES");
        guard
    }

    fn write_lines(path: &Path, lines: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        let mut file = fs::File::create(path).expect("create file");
        for index in 0..lines {
            writeln!(file, "// line {index}").expect("write line");
        }
    }

    fn create_workspace_with_sources() -> tempfile::TempDir {
        let dir = tempdir().expect("create workspace");
        write_lines(&dir.path().join("src/lib.rs"), 8);
        write_lines(&dir.path().join("src/bin/tool.rs"), 3);
        let target_dir = dir.path().join("target/debug");
        fs::create_dir_all(&target_dir).expect("create target directory");
        write_lines(&target_dir.join("ignored.rs"), 120);
        dir
    }

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, EnforceLimitsOptions::default());
    }

    #[test]
    fn parse_args_accepts_custom_limits() {
        let options = parse_args([
            OsString::from("--max-lines"),
            OsString::from("700"),
            OsString::from("--warn-lines"),
            OsString::from("500"),
        ])
        .expect("parse succeeds");
        assert_eq!(
            options,
            EnforceLimitsOptions {
                max_lines: Some(700),
                warn_lines: Some(500),
                config_path: None,
            }
        );
    }

    #[test]
    fn parse_args_accepts_config_path() {
        let options = parse_args([
            OsString::from("--config"),
            OsString::from("tools/line_limits.toml"),
        ])
        .expect("parse succeeds");
        assert_eq!(
            options,
            EnforceLimitsOptions {
                max_lines: None,
                warn_lines: None,
                config_path: Some(PathBuf::from("tools/line_limits.toml")),
            }
        );
    }

    #[test]
    fn parse_config_supports_overrides() {
        let config = parse_line_limits_config(
            r#"
default_max_lines = 1200
default_warn_lines = 1000

[[overrides]]
path = "src/lib.rs"
max_lines = 1500
warn_lines = 1400

[[overrides]]
path = "src/bin/main.rs"
warn_lines = 650
"#,
            Path::new("line_limits.toml"),
        )
        .expect("parse succeeds");

        assert_eq!(config.default_max_lines, Some(1200));
        assert_eq!(config.default_warn_lines, Some(1000));

        let primary = config
            .override_for(Path::new("src/lib.rs"))
            .expect("override present");
        assert_eq!(primary.max_lines, Some(1500));
        assert_eq!(primary.warn_lines, Some(1400));

        let secondary = config
            .override_for(Path::new("src/bin/main.rs"))
            .expect("override present");
        assert_eq!(secondary.max_lines, None);
        assert_eq!(secondary.warn_lines, Some(650));
    }

    #[test]
    fn parse_config_rejects_parent_directories() {
        let error = parse_line_limits_config(
            r#"
[[overrides]]
path = "../src/lib.rs"
max_lines = 900
"#,
            Path::new("line_limits.toml"),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("parent directory")
        ));
    }

    #[test]
    fn parse_args_rejects_invalid_values() {
        let error = parse_args([OsString::from("--max-lines"), OsString::from("0")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--max-lines")));
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_warn_exceeding_maximum() {
        let error = parse_args([
            OsString::from("--warn-lines"),
            OsString::from("800"),
            OsString::from("--max-lines"),
            OsString::from("700"),
        ])
        .unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("warn line limit")));
    }

    #[test]
    fn execute_succeeds_with_cli_limits() {
        let workspace = create_workspace_with_sources();
        let _env = cleared_limits_env();

        execute(
            workspace.path(),
            EnforceLimitsOptions {
                max_lines: Some(20),
                warn_lines: Some(10),
                config_path: None,
            },
        )
        .expect("execution succeeds");
    }

    #[test]
    fn execute_warns_without_failing_when_above_warn_threshold() {
        let workspace = create_workspace_with_sources();
        let _env = cleared_limits_env();

        execute(
            workspace.path(),
            EnforceLimitsOptions {
                max_lines: Some(12),
                warn_lines: Some(5),
                config_path: None,
            },
        )
        .expect("warnings do not fail execution");
    }

    #[test]
    fn execute_reports_error_when_exceeding_max_lines() {
        let workspace = create_workspace_with_sources();
        let _env = cleared_limits_env();

        let error = execute(
            workspace.path(),
            EnforceLimitsOptions {
                max_lines: Some(4),
                warn_lines: Some(3),
                config_path: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, TaskError::Validation(message) if message.contains("maximum line count"))
        );
    }

    #[test]
    fn execute_applies_config_overrides() {
        let workspace = create_workspace_with_sources();
        let config_path = workspace.path().join("tools/line_limits.toml");
        fs::create_dir_all(config_path.parent().unwrap()).expect("create config dir");
        fs::write(
            &config_path,
            r#"
default_max_lines = 100
default_warn_lines = 50

[[overrides]]
path = "src/bin/tool.rs"
max_lines = 2
warn_lines = 1
"#,
        )
        .expect("write config");

        let error = execute(
            workspace.path(),
            EnforceLimitsOptions {
                max_lines: None,
                warn_lines: None,
                config_path: Some(config_path.clone()),
            },
        )
        .unwrap_err();
        let message = match error {
            TaskError::Validation(message) => message,
            other => panic!("expected validation error, got {other:?}"),
        };
        assert!(message.contains("maximum line count"));
    }
}
