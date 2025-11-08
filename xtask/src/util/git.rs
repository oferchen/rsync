use super::env::map_command_error;
use crate::error::{TaskError, TaskResult};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Lists tracked files using `git ls-files -z`.
pub fn list_tracked_files(workspace: &Path) -> TaskResult<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "git ls-files",
                "install git and ensure it is available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("git ls-files"),
            status: output.status,
        });
    }

    Ok(parse_null_separated_paths(&output.stdout)?)
}

/// Returns all Rust sources tracked or untracked within the repository via git.
pub fn list_rust_sources_via_git(workspace: &Path) -> TaskResult<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "git ls-files",
                "install git and ensure it is available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("git ls-files"),
            status: output.status,
        });
    }

    let mut files = parse_null_separated_paths(&output.stdout)?;
    files.sort();
    files.dedup();
    Ok(files)
}

fn parse_null_separated_paths(data: &[u8]) -> TaskResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in data.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            files.push(PathBuf::from(OsString::from_vec(entry.to_vec())));
        }

        #[cfg(not(unix))]
        {
            let path = String::from_utf8(entry.to_vec()).map_err(|_| {
                TaskError::Metadata(String::from(
                    "git reported a non-UTF-8 path; binary audit requires UTF-8 file names on this platform",
                ))
            })?;
            files.push(PathBuf::from(path));
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::{list_rust_sources_via_git, list_tracked_files};
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    fn workspace_root() -> &'static Path {
        static ROOT: OnceLock<PathBuf> = OnceLock::new();
        ROOT.get_or_init(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        })
    }

    #[test]
    fn list_tracked_files_includes_manifest() {
        let files = list_tracked_files(workspace_root()).expect("git ls-files succeeds");
        assert!(files.iter().any(|path| path == Path::new("Cargo.toml")));
    }

    #[test]
    fn list_rust_sources_includes_xtask_main() {
        let files = list_rust_sources_via_git(workspace_root()).expect("git ls-files succeeds");
        assert!(
            files
                .iter()
                .any(|path| path == Path::new("xtask/src/main.rs"))
        );
    }
}
