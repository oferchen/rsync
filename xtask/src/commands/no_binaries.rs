use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, is_probably_binary, list_tracked_files};
use std::ffi::OsString;
use std::path::Path;

/// Options accepted by the `no-binaries` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoBinariesOptions;

/// Parses CLI arguments for the `no-binaries` command.
pub fn parse_args<I>(args: I) -> TaskResult<NoBinariesOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for no-binaries command",
            arg.to_string_lossy()
        )));
    }

    Ok(NoBinariesOptions)
}

/// Executes the `no-binaries` command.
pub fn execute(workspace: &Path, _options: NoBinariesOptions) -> TaskResult<()> {
    let tracked_files = list_tracked_files(workspace)?;
    let mut binary_paths = Vec::new();

    for relative in tracked_files {
        let absolute = workspace.join(&relative);
        if is_probably_binary(&absolute)? {
            binary_paths.push(relative);
        }
    }

    if binary_paths.is_empty() {
        println!("No tracked binary files detected.");
        return Ok(());
    }

    binary_paths.sort();
    Err(TaskError::BinaryFiles(binary_paths))
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask no-binaries\n\nOptions:\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, NoBinariesOptions);
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("no-binaries")));
    }
}
