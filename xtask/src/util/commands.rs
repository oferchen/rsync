use super::env::{map_command_error, should_simulate_missing_tool, tool_missing_error};
use crate::error::{TaskError, TaskResult};
use std::env;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// Command-object wrapper for invoking `cargo` in a given workspace.
///
/// This encapsulates the common setup for cargo invocations (workspace, args,
/// env overrides, display string, install hint) so higher-level helpers can
/// focus on interpreting the result instead of wiring up `Command` every time.
struct CargoCommand<'a> {
    workspace: &'a Path,
    args: Vec<OsString>,
    env_overrides: Vec<(OsString, OsString)>,
    display: &'a str,
    install_hint: &'a str,
}

impl<'a> CargoCommand<'a> {
    fn new(workspace: &'a Path, display: &'a str, install_hint: &'a str) -> Self {
        Self {
            workspace,
            args: Vec::new(),
            env_overrides: Vec::new(),
            display,
            install_hint,
        }
    }

    fn with_args(mut self, args: Vec<OsString>) -> Self {
        self.args = args;
        self
    }

    fn with_env_overrides(mut self, env_overrides: &[(OsString, OsString)]) -> Self {
        self.env_overrides = env_overrides.to_vec();
        self
    }

    fn execute(self) -> TaskResult<Output> {
        if should_simulate_missing_tool(self.display) {
            return Err(tool_missing_error(self.display, self.install_hint));
        }

        let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        Command::new(cargo)
            .current_dir(self.workspace)
            .args(&self.args)
            .envs(
                self.env_overrides
                    .iter()
                    .map(|(key, value)| (key.as_os_str(), value.as_os_str())),
            )
            .output()
            .map_err(|error| map_command_error(error, self.display, self.install_hint))
    }
}

/// Shared mapper for failed cargo invocations.
///
/// This centralizes the "no such subcommand" translation into `ToolMissing` and
/// otherwise returns a `CommandFailed` error.
fn map_cargo_failure(
    display: &str,
    install_hint: &str,
    output: Output,
    program_label: Option<String>,
) -> TaskError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no such subcommand") || stderr.contains("no such command") {
        tool_missing_error(display, install_hint)
    } else {
        TaskError::CommandFailed {
            program: program_label.unwrap_or_else(|| display.to_string()),
            status: output.status,
        }
    }
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
    const LIST_DISPLAY: &str = "rustup target list --installed";
    const ADD_DISPLAY: &str = "rustup target add";
    let install_hint = format!("install the '{target}' target with `rustup target add {target}`");

    if should_simulate_missing_tool(LIST_DISPLAY) {
        return Err(tool_missing_error(LIST_DISPLAY, &install_hint));
    }

    ensure_command_available(
        "rustup",
        "install rustup from https://rustup.rs to manage Rust toolchains",
    )?;

    let query = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|error| map_command_error(error, LIST_DISPLAY, &install_hint))?;

    if !query.status.success() {
        return Err(TaskError::CommandFailed {
            program: LIST_DISPLAY.to_string(),
            status: query.status,
        });
    }

    let installed = String::from_utf8_lossy(&query.stdout);
    if installed.lines().any(|line| line.trim() == target) {
        return Ok(());
    }

    if should_simulate_missing_tool(ADD_DISPLAY) {
        return Err(tool_missing_error(ADD_DISPLAY, &install_hint));
    }

    println!("Installing missing Rust target {target} with `rustup target add {target}`");

    let status = Command::new("rustup")
        .args(["target", "add", target])
        .status()
        .map_err(|error| map_command_error(error, ADD_DISPLAY, &install_hint))?;

    if !status.success() {
        return Err(TaskError::CommandFailed {
            program: format!("{ADD_DISPLAY} {target}"),
            status,
        });
    }

    Ok(())
}

/// Runs `cargo` with the supplied arguments and maps failures to [`TaskError`].
pub fn run_cargo_tool(
    workspace: &Path,
    args: Vec<OsString>,
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    run_cargo_tool_with_env(workspace, args, &[], display, install_hint)
}

pub fn run_cargo_tool_with_env(
    workspace: &Path,
    args: Vec<OsString>,
    env_overrides: &[(OsString, OsString)],
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    let output = CargoCommand::new(workspace, display, install_hint)
        .with_args(args)
        .with_env_overrides(env_overrides)
        .execute()?;

    if output.status.success() {
        return Ok(());
    }

    // Surface stdout/stderr for diagnostics (especially useful in CI on Windows).
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("{display} failed with status {}", output.status);
    if !stdout.trim().is_empty() {
        eprintln!("----- {display} stdout -----");
        eprintln!("{stdout}");
    }
    if !stderr.trim().is_empty() {
        eprintln!("----- {display} stderr -----");
        eprintln!("{stderr}");
    }

    Err(map_cargo_failure(display, install_hint, output, None))
}

/// Probes a cargo subcommand without executing the full task, returning a
/// [`TaskError::ToolMissing`] when the tool is unavailable.
pub fn probe_cargo_tool(
    workspace: &Path,
    args: &[&str],
    display: &str,
    install_hint: &str,
) -> TaskResult<()> {
    // Reuse the same Command Object for probing; only the failure mapping differs.
    let args_os: Vec<OsString> = args.iter().map(|arg| OsString::from(*arg)).collect();

    let output = CargoCommand::new(workspace, display, install_hint)
        .with_args(args_os)
        .execute()?;

    if output.status.success() {
        return Ok(());
    }

    // For probes, keep the slightly different label used in existing tests.
    let program_label = Some(format!("{display} (probe)"));
    Err(map_cargo_failure(
        display,
        install_hint,
        output,
        program_label,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_command_available, ensure_rust_target_installed, probe_cargo_tool, run_cargo_tool,
    };
    use crate::error::TaskError;
    use crate::util::env::FORCE_MISSING_ENV;
    use std::env;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};

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
            unsafe {
                env::set_var(key, value);
            }
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
                unsafe {
                    env::set_var(self.key, previous);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
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
        assert!(matches!(
            err,
            TaskError::ToolMissing(message) if message.contains("nonexistent-subcommand")
        ));
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
        assert!(matches!(
            err,
            TaskError::ToolMissing(message) if message.contains("cargo --version")
        ));
    }

    #[test]
    fn ensure_command_available_honours_missing_path_entries() {
        assert!(
            ensure_command_available("cargo", "install cargo").is_ok(),
            "cargo should be available in CI"
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

    #[test]
    fn ensure_rust_target_installed_respects_missing_add_command() {
        let _guard = EnvGuard::set(FORCE_MISSING_ENV, "rustup target add");
        let error = ensure_rust_target_installed("nonexistent-target").unwrap_err();
        assert!(matches!(
            error,
            TaskError::ToolMissing(message) if message.contains("rustup target add")
        ));
    }

    #[test]
    fn probe_cargo_tool_maps_missing_subcommand() {
        let err = probe_cargo_tool(
            workspace_root(),
            &["nonexistent-subcommand"],
            "cargo nonexistent-subcommand",
            "install the missing tool",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            TaskError::ToolMissing(message) if message.contains("nonexistent-subcommand")
        ));
    }
}
