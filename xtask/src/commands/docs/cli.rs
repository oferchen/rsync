use crate::error::{TaskError, TaskResult};
use crate::util::is_help_flag;
use std::ffi::OsString;

/// Options accepted by the `docs` command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocsOptions {
    /// Whether to open the generated documentation in a browser.
    pub open: bool,
    /// Whether to validate documentation snippets against workspace branding metadata.
    pub validate: bool,
}

/// Parses CLI arguments for the `docs` command.
pub fn parse_args<I>(args: I) -> TaskResult<DocsOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = DocsOptions::default();

    for arg in args {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        if arg == "--open" {
            if options.open {
                return Err(TaskError::Usage(String::from(
                    "--open specified multiple times",
                )));
            }
            options.open = true;
            continue;
        }

        if arg == "--validate" {
            if options.validate {
                return Err(TaskError::Usage(String::from(
                    "--validate specified multiple times",
                )));
            }
            options.validate = true;
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for docs command",
            arg.to_string_lossy()
        )));
    }

    Ok(options)
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask docs [--open] [--validate]\n\nOptions:\n  --open          Open documentation after building\n  --validate     Validate branding references in Markdown documents\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, DocsOptions::default());
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_duplicate_open_flags() {
        let error = parse_args([OsString::from("--open"), OsString::from("--open")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--open")));
    }

    #[test]
    fn parse_args_accepts_validate_flag() {
        let options =
            parse_args([OsString::from("--validate")]).expect("parse succeeds with validate");
        assert!(options.validate);
        assert!(!options.open);
    }

    #[test]
    fn parse_args_rejects_duplicate_validate_flags() {
        let error =
            parse_args([OsString::from("--validate"), OsString::from("--validate")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--validate")));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    #[test]
    fn usage_text_is_exposed_through_module_interface() {
        assert!(crate::commands::docs::usage().contains("Usage:"));
    }
}
