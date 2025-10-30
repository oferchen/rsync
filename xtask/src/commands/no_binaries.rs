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
    use std::ffi::OsString;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_git_repo(path: &std::path::Path) {
        let status = Command::new("git")
            .current_dir(path)
            .args(["init", "--quiet"])
            .status()
            .expect("git init runs");
        assert!(status.success(), "git init failed: {:?}", status);
    }

    fn git_add(path: &std::path::Path, file: &std::path::Path) {
        let relative = file.strip_prefix(path).expect("file inside repo");
        let status = Command::new("git")
            .current_dir(path)
            .arg("add")
            .arg(relative)
            .status()
            .expect("git add runs");
        assert!(
            status.success(),
            "git add failed for {:?}: {:?}",
            relative,
            status
        );
    }

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

    #[test]
    fn execute_succeeds_when_all_tracked_files_are_textual() {
        let temp = tempdir().expect("tempdir");
        init_git_repo(temp.path());

        let source = temp.path().join("src/lib.rs");
        fs::create_dir_all(source.parent().unwrap()).expect("create src dir");
        fs::write(&source, "fn main() {}\n").expect("write source file");
        git_add(temp.path(), &source);

        execute(temp.path(), NoBinariesOptions).expect("no binary files detected");
    }

    #[test]
    fn execute_reports_detected_binary_files() {
        let temp = tempdir().expect("tempdir");
        init_git_repo(temp.path());

        let text_file = temp.path().join("README.md");
        fs::write(&text_file, "workspace docs\n").expect("write text file");
        git_add(temp.path(), &text_file);

        let binary_file = temp.path().join("artifacts/blob.bin");
        fs::create_dir_all(binary_file.parent().unwrap()).expect("create dir");
        fs::write(&binary_file, [0_u8, 1, 2, 3, 4]).expect("write binary file");
        git_add(temp.path(), &binary_file);

        let error = execute(temp.path(), NoBinariesOptions).unwrap_err();
        match error {
            TaskError::BinaryFiles(paths) => {
                assert_eq!(paths, vec![std::path::PathBuf::from("artifacts/blob.bin")]);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }
}
