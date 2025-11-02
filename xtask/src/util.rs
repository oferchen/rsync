use crate::error::{TaskError, TaskResult};
use serde_json::Value as JsonValue;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

/// Returns `true` when the argument requests help output.
pub fn is_help_flag(value: &OsString) -> bool {
    value == "--help" || value == "-h"
}

/// Ensures the provided condition holds, returning a [`TaskError::Validation`]
/// otherwise.
pub fn ensure(condition: bool, message: impl Into<String>) -> TaskResult<()> {
    if condition {
        Ok(())
    } else {
        Err(validation_error(message))
    }
}

/// Constructs a [`TaskError::Validation`] using the provided message.
pub fn validation_error(message: impl Into<String>) -> TaskError {
    TaskError::Validation(message.into())
}

const FORCE_MISSING_ENV: &str = "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS";

fn should_simulate_missing_tool(display: &str) -> bool {
    let Ok(entries) = env::var(FORCE_MISSING_ENV) else {
        return false;
    };

    entries
        .split(|ch| [',', ';', '|'].contains(&ch))
        .map(str::trim)
        .any(|value| !value.is_empty() && value == display)
}

fn map_command_error(error: io::Error, program: &str, install_hint: &str) -> TaskError {
    if error.kind() == io::ErrorKind::NotFound {
        TaskError::ToolMissing(format!("{program} is unavailable; {install_hint}"))
    } else {
        TaskError::Io(error)
    }
}

fn tool_missing_error(display: &str, install_hint: &str) -> TaskError {
    TaskError::ToolMissing(format!("{display} is unavailable; {install_hint}"))
}

/// Ensures that the provided command is present in `PATH`.
pub fn ensure_command_available(program: &str, install_hint: &str) -> TaskResult<()> {
    if should_simulate_missing_tool(program) {
        return Err(tool_missing_error(program, install_hint));
    }

    let path_value = env::var_os("PATH").unwrap_or_default();
    let mut candidates = vec![OsString::from(program)];
    let exe_suffix = env::consts::EXE_SUFFIX;
    if !exe_suffix.is_empty() && !program.ends_with(exe_suffix) {
        candidates.push(OsString::from(format!("{program}{exe_suffix}")));
    }

    for directory in env::split_paths(&path_value) {
        for candidate in &candidates {
            let path = directory.join(candidate);
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    #[cfg(unix)]
                    {
                        if metadata.permissions().mode() & 0o111 == 0 {
                            continue;
                        }
                    }

                    return Ok(());
                }
                Ok(_) | Err(_) => {
                    continue;
                }
            }
        }
    }

    Err(tool_missing_error(program, install_hint))
}

/// Ensures that the requested Rust target triple is installed via `rustup`.
pub fn ensure_rust_target_installed(target: &str) -> TaskResult<()> {
    const DISPLAY: &str = "rustup target list --installed";
    let install_hint = format!("install the '{target}' target with `rustup target add {target}`");

    if should_simulate_missing_tool(DISPLAY) {
        return Err(tool_missing_error(DISPLAY, &install_hint));
    }

    ensure_command_available(
        "rustup",
        "install rustup from https://rustup.rs to manage Rust toolchains",
    )?;

    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|error| map_command_error(error, DISPLAY, &install_hint))?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: DISPLAY.to_string(),
            status: output.status,
        });
    }

    let installed = String::from_utf8_lossy(&output.stdout);
    if installed.lines().any(|line| line.trim() == target) {
        return Ok(());
    }

    Err(TaskError::ToolMissing(format!(
        "Rust target {target} is not installed; run `rustup target add {target}`",
    )))
}

/// Runs `cargo` with the supplied arguments and maps failures to [`TaskError`].
pub fn run_cargo_tool(
    workspace: &std::path::Path,
    args: Vec<OsString>,
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    run_cargo_tool_with_env(workspace, args, &[], display, install_hint)
}

pub fn run_cargo_tool_with_env(
    workspace: &std::path::Path,
    args: Vec<OsString>,
    env: &[(OsString, OsString)],
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    if should_simulate_missing_tool(display) {
        return Err(tool_missing_error(display, install_hint));
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args(&args)
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_os_str(), value.as_os_str())),
        )
        .output()
        .map_err(|error| map_command_error(error, display, install_hint))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no such subcommand") || stderr.contains("no such command") {
        return Err(tool_missing_error(display, install_hint));
    }

    Err(TaskError::CommandFailed {
        program: display.to_string(),
        status: output.status,
    })
}

/// Probes a cargo subcommand without executing the full task, returning a
/// [`TaskError::ToolMissing`] when the tool is unavailable.
pub fn probe_cargo_tool(
    workspace: &Path,
    args: &[&str],
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    if should_simulate_missing_tool(display) {
        return Err(tool_missing_error(display, install_hint));
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| map_command_error(error, display, install_hint))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no such subcommand") || stderr.contains("no such command") {
        Err(tool_missing_error(display, install_hint))
    } else {
        Err(TaskError::CommandFailed {
            program: format!("{display} (probe)"),
            status: output.status,
        })
    }
}

/// Lists tracked files using `git ls-files -z`.
pub fn list_tracked_files(workspace: &std::path::Path) -> TaskResult<Vec<PathBuf>> {
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

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
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

/// Returns all Rust sources tracked or untracked within the repository via git.
pub fn list_rust_sources_via_git(workspace: &std::path::Path) -> TaskResult<Vec<PathBuf>> {
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

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
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
                    "git reported a non-UTF-8 path; placeholder scanning requires UTF-8 file names on this platform",
                ))
            })?;
            files.push(PathBuf::from(path));
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

/// Heuristically determines whether the path references a binary file.
pub fn is_probably_binary(path: &std::path::Path) -> TaskResult<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(false);
    }

    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let mut printable = 0usize;
    let mut control = 0usize;
    let mut inspected = 0usize;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        inspected += read;

        for &byte in &buffer[..read] {
            match byte {
                0 => return Ok(true),
                0x07 | 0x08 | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => printable += 1,
                0x20..=0x7E => printable += 1,
                _ if byte >= 0x80 => printable += 1,
                _ => control += 1,
            }
        }

        if control > printable {
            return Ok(true);
        }

        if inspected >= buffer.len() {
            break;
        }
    }

    Ok(false)
}

/// Reads JSON metadata from `cargo metadata`.
pub fn cargo_metadata_json(workspace: &std::path::Path) -> TaskResult<JsonValue> {
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "cargo metadata",
                "ensure cargo is installed and available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("cargo metadata"),
            status: output.status,
        });
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        TaskError::Metadata(format!("failed to parse cargo metadata JSON: {error}"))
    })
}

/// Counts the number of lines in the provided UTF-8 text file.
pub fn count_file_lines(path: &std::path::Path) -> TaskResult<usize> {
    let file = fs::File::open(path)?;
    let mut reader = io::BufReader::new(file);
    let mut buffer = String::new();
    let mut count = 0usize;

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += 1;
    }

    Ok(count)
}

/// Reads an environment variable that stores a positive integer, returning
/// `Ok(None)` when the variable is not set.
pub fn read_limit_env_var(name: &str) -> TaskResult<Option<usize>> {
    match env::var(name) {
        Ok(value) => {
            if value.is_empty() {
                return Err(TaskError::Validation(format!(
                    "{name} must be a positive integer, found an empty value"
                )));
            }

            let parsed = parse_positive_usize_from_env(name, &value)?;
            Ok(Some(parsed))
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(TaskError::Validation(format!(
            "{name} must contain a UTF-8 encoded positive integer"
        ))),
    }
}

fn parse_positive_usize_from_env(name: &str, value: &str) -> TaskResult<usize> {
    let parsed = value.parse::<usize>().map_err(|_| {
        TaskError::Validation(format!(
            "{name} must be a positive integer, found '{value}'"
        ))
    })?;

    if parsed == 0 {
        return Err(TaskError::Validation(format!(
            "{name} must be greater than zero, found '{value}'"
        )));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TaskError;
    use std::io::Write;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn workspace_root() -> &'static Path {
        static ROOT: OnceLock<PathBuf> = OnceLock::new();
        ROOT.get_or_init(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        })
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &str) -> Self {
            let guard = env_lock().lock().unwrap();
            let previous = env::var_os(key);
            unsafe { env::set_var(key, value) };
            Self {
                key,
                previous,
                _lock: guard,
            }
        }

        #[allow(unsafe_code)]
        fn remove(key: &'static str) -> Self {
            let guard = env_lock().lock().unwrap();
            let previous = env::var_os(key);
            unsafe { env::remove_var(key) };
            Self {
                key,
                previous,
                _lock: guard,
            }
        }
    }

    impl Drop for EnvGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                unsafe { env::set_var(self.key, previous) };
            } else {
                unsafe { env::remove_var(self.key) };
            }
        }
    }

    #[test]
    fn help_flag_detection_matches_short_and_long_forms() {
        assert!(is_help_flag(&OsString::from("--help")));
        assert!(is_help_flag(&OsString::from("-h")));
        assert!(!is_help_flag(&OsString::from("--HELP")));
    }

    #[test]
    fn ensure_reports_validation_failure() {
        ensure(true, "unused message").expect("true condition succeeds");
        let error = ensure(false, "failure").unwrap_err();
        assert!(matches!(error, TaskError::Validation(message) if message == "failure"));
    }

    #[test]
    fn validation_error_constructs_validation_variant() {
        let error = validation_error("invalid");
        assert!(matches!(error, TaskError::Validation(message) if message == "invalid"));
    }

    #[test]
    fn run_cargo_tool_succeeds_for_version_query() {
        run_cargo_tool(
            workspace_root(),
            vec![OsString::from("--version")],
            "cargo --version",
            "install cargo",
        )
        .expect("cargo --version succeeds");
    }

    #[test]
    fn run_cargo_tool_maps_missing_subcommand_to_tool_missing() {
        let err = run_cargo_tool(
            workspace_root(),
            vec![OsString::from("nonexistent-subcommand")],
            "cargo nonexistent-subcommand",
            "install the missing tool",
        )
        .unwrap_err();
        assert!(
            matches!(err, TaskError::ToolMissing(message) if message.contains("nonexistent-subcommand"))
        );
    }

    #[test]
    fn run_cargo_tool_honours_forced_missing_configuration() {
        let _env = EnvGuard::set(FORCE_MISSING_ENV, "cargo --version");
        let err = run_cargo_tool(
            workspace_root(),
            vec![OsString::from("--version")],
            "cargo --version",
            "install cargo",
        )
        .unwrap_err();
        assert!(
            matches!(err, TaskError::ToolMissing(message) if message.contains("cargo --version"))
        );
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

    #[test]
    fn binary_detection_flags_control_bytes() {
        let dir = tempdir().expect("create temp dir");
        let text_path = dir.path().join("text.rs");
        fs::write(&text_path, b"fn main() {}\n").expect("write text file");
        assert!(!is_probably_binary(&text_path).expect("check succeeds"));

        let binary_path = dir.path().join("binary.bin");
        let mut file = fs::File::create(&binary_path).expect("create binary file");
        file.write_all(b"\x00\x01\x02not ascii")
            .expect("write binary");
        drop(file);
        assert!(is_probably_binary(&binary_path).expect("check succeeds"));
    }

    #[test]
    fn cargo_metadata_json_loads_workspace_metadata() {
        let metadata = cargo_metadata_json(workspace_root()).expect("metadata loads");
        assert!(metadata.get("packages").is_some());
    }

    #[test]
    fn cargo_metadata_json_reports_failure() {
        let dir = tempdir().expect("create temp dir");
        let err = cargo_metadata_json(dir.path()).unwrap_err();
        assert!(
            matches!(err, TaskError::CommandFailed { program, .. } if program == "cargo metadata")
        );
    }

    #[test]
    fn count_file_lines_handles_various_lengths() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("source.rs");
        fs::write(&file_path, "line one\nline two\nline three").expect("write file");
        assert_eq!(count_file_lines(&file_path).expect("count succeeds"), 3);
    }

    #[test]
    fn read_limit_env_var_parses_positive_values() {
        let _guard = EnvGuard::set("TEST_LIMIT", "42");
        assert_eq!(
            read_limit_env_var("TEST_LIMIT").expect("read succeeds"),
            Some(42)
        );
    }

    #[test]
    fn read_limit_env_var_handles_missing_and_invalid_values() {
        {
            let _guard = EnvGuard::remove("MISSING_LIMIT");
            assert!(
                read_limit_env_var("MISSING_LIMIT")
                    .expect("missing is ok")
                    .is_none()
            );
        }

        {
            let _zero = EnvGuard::set("ZERO_LIMIT", "0");
            let zero_err = read_limit_env_var("ZERO_LIMIT").unwrap_err();
            assert!(
                matches!(zero_err, TaskError::Validation(message) if message.contains("ZERO_LIMIT"))
            );
        }

        let _invalid = EnvGuard::set("BAD_LIMIT", "not-a-number");
        let invalid_err = read_limit_env_var("BAD_LIMIT").unwrap_err();
        assert!(
            matches!(invalid_err, TaskError::Validation(message) if message.contains("BAD_LIMIT"))
        );
    }

    #[test]
    fn parse_positive_usize_from_env_rejects_zero_and_negative() {
        let err = parse_positive_usize_from_env("VALUE", "0").unwrap_err();
        assert!(matches!(err, TaskError::Validation(message) if message.contains("VALUE")));

        let err = parse_positive_usize_from_env("VALUE", "-1").unwrap_err();
        assert!(matches!(err, TaskError::Validation(message) if message.contains("VALUE")));

        assert_eq!(
            parse_positive_usize_from_env("VALUE", "7").expect("parse succeeds"),
            7
        );
    }

    #[test]
    fn ensure_rust_target_installed_accepts_available_targets() {
        let output = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()
            .expect("query installed rustup targets");
        assert!(output.status.success(), "rustup reported an error");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let target = stdout
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .expect("at least one target installed");
        ensure_rust_target_installed(target)
            .unwrap_or_else(|error| panic!("target {target} should be installed: {error:?}"));
    }

    #[test]
    fn ensure_rust_target_installed_respects_forced_missing_env() {
        let _guard = EnvGuard::set(FORCE_MISSING_ENV, "rustup target list --installed");
        let error = ensure_rust_target_installed("x86_64-unknown-linux-gnu").unwrap_err();
        assert!(matches!(
            error,
            TaskError::ToolMissing(message) if message.contains("rustup target list --installed")
        ));
    }
}
