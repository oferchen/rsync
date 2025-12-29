#[cfg(test)]
use crate::error::{TaskError, TaskResult};
#[cfg(test)]
use crate::util::is_help_flag;
#[cfg(test)]
use std::ffi::OsString;

/// Output format supported by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrandingOutputFormat {
    /// Human-readable text report.
    #[default]
    Text,
    /// Structured JSON report suitable for automation.
    Json,
}

/// Options accepted by the `branding` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrandingOptions {
    /// Desired output format.
    pub format: BrandingOutputFormat,
}

/// Parses CLI arguments for the `branding` command.
#[cfg(test)]
pub fn parse_args<I>(args: I) -> TaskResult<BrandingOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = BrandingOptions::default();

    for arg in args.into_iter() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        let Some(raw) = arg.to_str() else {
            return Err(TaskError::Usage(String::from(
                "branding command arguments must be valid UTF-8",
            )));
        };

        match raw {
            "--json" => {
                if !matches!(options.format, BrandingOutputFormat::Text) {
                    return Err(TaskError::Usage(String::from(
                        "--json specified multiple times",
                    )));
                }
                options.format = BrandingOutputFormat::Json;
            }
            _ => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{raw}' for branding command",
                )));
            }
        }
    }

    Ok(options)
}

/// Returns usage text for the command.
#[cfg(test)]
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask branding [--json]\n\nOptions:\n  --json          Emit branding metadata in JSON format\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, BrandingOptions::default());
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_enables_json_output() {
        let options = parse_args([OsString::from("--json")]).expect("parse succeeds");
        assert_eq!(
            options,
            BrandingOptions {
                format: BrandingOutputFormat::Json,
            }
        );
    }

    #[test]
    fn parse_args_rejects_duplicate_json_flags() {
        let error = parse_args([OsString::from("--json"), OsString::from("--json")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--json")));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }
}
