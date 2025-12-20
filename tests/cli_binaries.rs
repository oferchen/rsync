use core::fallback::CLIENT_FALLBACK_ENV;
use core::version::{
    COPYRIGHT_NOTICE, DAEMON_PROGRAM_NAME, HIGHEST_PROTOCOL_VERSION, LEGACY_DAEMON_PROGRAM_NAME,
    LEGACY_PROGRAM_NAME, PROGRAM_NAME, RUST_VERSION, SOURCE_URL,
};
use std::collections::{BTreeSet, HashSet};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn unique_program_names(names: &[&'static str]) -> Vec<&'static str> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();

    for name in names.iter().copied() {
        if seen.insert(name) {
            unique.push(name);
        }
    }

    unique
}

fn client_binaries() -> Vec<&'static str> {
    unique_program_names(&[PROGRAM_NAME, LEGACY_PROGRAM_NAME])
}

fn daemon_binaries() -> Vec<&'static str> {
    unique_program_names(&[DAEMON_PROGRAM_NAME, LEGACY_DAEMON_PROGRAM_NAME])
}

fn binary_output(name: &str, args: &[&str]) -> Output {
    let mut command = binary_command(name);
    command.args(args);
    command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {name}: {error}"))
}

fn combined_utf8(output: &std::process::Output) -> String {
    let mut data = output.stdout.clone();
    data.extend_from_slice(&output.stderr);
    String::from_utf8(data).expect("binary output should be valid UTF-8")
}

#[test]
fn client_help_lists_usage() {
    for binary in client_binaries() {
        if locate_binary(binary).is_none() {
            if binary == PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &["--help"]);
        assert!(output.status.success(), "{binary} --help should succeed");
        assert!(
            output.stderr.is_empty(),
            "{binary} help output should not write to stderr"
        );
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains("Usage:"),
            "{binary} help output should contain a Usage: line, got:\n{stdout}"
        );
        assert!(
            stdout.contains(binary),
            "{binary} help output should mention the binary name, got:\n{stdout}"
        );
    }
}

#[test]
fn client_without_operands_shows_usage() {
    for binary in client_binaries() {
        if locate_binary(binary).is_none() {
            if binary == PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &[]);
        assert!(
            !output.status.success(),
            "running {binary} without operands should fail so the caller sees the usage"
        );
        let combined = combined_utf8(&output);
        assert!(
            combined.contains("Usage:"),
            "{binary} output without operands should contain Usage:, got:\n{combined}"
        );
    }
}

#[test]
fn daemon_help_lists_usage() {
    for binary in daemon_binaries() {
        if locate_binary(binary).is_none() {
            if binary == DAEMON_PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &["--daemon", "--help"]);
        assert!(
            output.status.success(),
            "{binary} --daemon --help should succeed"
        );
        assert!(
            output.stderr.is_empty(),
            "{binary} daemon help output should not write to stderr"
        );
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains("Usage:"),
            "{binary} daemon help output should contain a Usage: line, got:\n{stdout}"
        );
        assert!(
            stdout.contains(binary),
            "{binary} daemon help output should mention the binary name, got:\n{stdout}"
        );
    }
}

#[test]
fn client_version_reports_branding_metadata() {
    for binary in client_binaries() {
        if locate_binary(binary).is_none() {
            if binary == PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &["--version"]);
        assert!(output.status.success(), "{binary} --version should succeed");
        assert!(
            output.stderr.is_empty(),
            "{binary} version output should not write to stderr"
        );
        let combined = combined_utf8(&output);

        assert!(
            combined.contains(&format!("{binary} v{RUST_VERSION}")),
            "{binary} version banner must include the Rust branded version, got:\n{combined}"
        );
        assert!(
            combined.contains(&format!("protocol version {HIGHEST_PROTOCOL_VERSION}")),
            "{binary} version banner must report the negotiated protocol, got:\n{combined}"
        );
        assert!(
            combined.contains(SOURCE_URL),
            "{binary} version banner must advertise the project source URL, got:\n{combined}"
        );
        assert!(
            combined.contains(COPYRIGHT_NOTICE),
            "{binary} version banner must include the copyright notice, got:\n{combined}"
        );
    }
}

#[test]
fn daemon_version_reports_branding_metadata() {
    for binary in daemon_binaries() {
        if locate_binary(binary).is_none() {
            if binary == DAEMON_PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &["--daemon", "--version"]);
        assert!(
            output.status.success(),
            "{binary} --daemon --version should succeed"
        );
        assert!(
            output.stderr.is_empty(),
            "{binary} daemon version output should not write to stderr"
        );
        let combined = combined_utf8(&output);

        assert!(
            combined.contains(&format!("{binary} v{RUST_VERSION}")),
            "{binary} daemon version banner must include the Rust branded version, got:\n{combined}"
        );
        assert!(
            combined.contains(&format!("protocol version {HIGHEST_PROTOCOL_VERSION}")),
            "{binary} daemon version banner must report the negotiated protocol, got:\n{combined}"
        );
        assert!(
            combined.contains(SOURCE_URL),
            "{binary} daemon version banner must advertise the project source URL, got:\n{combined}"
        );
        assert!(
            combined.contains(COPYRIGHT_NOTICE),
            "{binary} daemon version banner must include the copyright notice, got:\n{combined}"
        );
    }
}

#[test]
fn daemon_rejects_unknown_flag() {
    for binary in daemon_binaries() {
        if locate_binary(binary).is_none() {
            if binary == DAEMON_PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &["--daemon", "--definitely-not-a-flag"]);
        assert!(
            !output.status.success(),
            "unexpected flags should return a failure exit status for {binary} in daemon mode"
        );
        let combined = combined_utf8(&output);
        assert!(
            combined.contains("unknown option"),
            "{binary} daemon output for unknown flag should mention \"unknown option\", got:\n{combined}"
        );
    }
}

#[cfg(unix)]
#[test]
#[ignore = "Obsolete: Server now uses native implementation instead of fallback delegation (gatekeeper bug fix)"]
fn server_entry_execs_fallback_binary() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let binary = locate_binary(PROGRAM_NAME)
        .unwrap_or_else(|| panic!("expected {PROGRAM_NAME} binary to be available"));

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server_exec.sh");
    let marker_path = temp.path().join("marker.txt");

    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
: "${SERVER_MARKER:?}"
printf 'server stdout from exec\n'
printf 'server stderr from exec\n' >&2
printf 'executed' > "$SERVER_MARKER"
exit 42
"#,
    )
    .expect("write script");

    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set script perms");

    let mut command = if let Some(runner) = cargo_target_runner() {
        let mut iter = runner.into_iter();
        let mut runner_command = Command::new(iter.next().expect("runner binary"));
        runner_command.args(iter);
        runner_command.arg(&binary);
        runner_command
    } else {
        Command::new(&binary)
    };

    command.args(["--server", "--sender", ".", "dest"]);
    command.env(CLIENT_FALLBACK_ENV, &script_path);
    command.env("SERVER_MARKER", &marker_path);

    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {PROGRAM_NAME}: {error}"));

    assert_eq!(output.status.code(), Some(42));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("server stdout from exec"),
        "stdout should come from fallback exec, got: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("server stderr from exec"),
        "stderr should come from fallback exec, got: {stderr}"
    );
    assert_eq!(
        fs::read(&marker_path).expect("read marker"),
        b"executed",
        "marker file should be written by fallback exec"
    );
}

fn binary_command(name: &str) -> Command {
    let binary = locate_binary(name)
        .unwrap_or_else(|| panic!("failed to locate {name} binary for integration testing"));

    if let Some(runner) = cargo_target_runner() {
        let mut runner_iter = runner.into_iter();
        let runner_binary = runner_iter
            .next()
            .unwrap_or_else(|| panic!("{name} runner command is empty"));
        let mut command = Command::new(runner_binary);
        command.args(runner_iter);
        command.arg(&binary);
        command
    } else {
        Command::new(binary)
    }
}

/// Locate a test binary built by Cargo.
///
/// Resolution order:
/// 1. `CARGO_BIN_EXE_<name>` environment variable set by Cargo.
/// 2. Walk upwards from the current executable looking for a `target`
///    directory and common `{debug,release}` subdirectories.
/// 3. Fallback to a sibling of the current executable, stripping a trailing
///    `deps/` segment when present.
///
/// System-wide `PATH` is **intentionally ignored** to avoid picking up host
/// binaries (for example, `/usr/bin/rsync`) that do not match oc-rsync
/// behaviour, which would make the tests environment-dependent.
fn locate_binary(name: &str) -> Option<PathBuf> {
    let env_var = format!("CARGO_BIN_EXE_{name}");
    if let Some(path) = env::var_os(&env_var) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let current_exe = env::current_exe().ok()?;
    let mut candidates = BTreeSet::new();

    let mut directory = current_exe.parent();
    while let Some(dir) = directory {
        candidates.insert(dir.join(&binary_name));

        if dir
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == "target")
        {
            candidates.insert(dir.join("debug").join(&binary_name));
            candidates.insert(dir.join("release").join(&binary_name));

            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Ok(file_type) = entry.file_type()
                        && file_type.is_dir()
                    {
                        let entry_path = entry.path();
                        candidates.insert(entry_path.join(&binary_name));
                        candidates.insert(entry_path.join("debug").join(&binary_name));
                        candidates.insert(entry_path.join("release").join(&binary_name));
                    }
                }
            }
        }

        directory = dir.parent();
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Fallback: resolve beside the test binary (e.g. target/debug/oc-rsync)
    let mut fallback_dir = current_exe;
    fallback_dir.pop();
    if fallback_dir.ends_with("deps") {
        fallback_dir.pop();
    }
    fallback_dir.push(binary_name);
    if fallback_dir.is_file() {
        Some(fallback_dir)
    } else {
        None
    }
}

fn cargo_target_runner() -> Option<Vec<String>> {
    let target = env::var("TARGET").ok()?;
    let runner_env = format!(
        "CARGO_TARGET_{}_RUNNER",
        target.replace('-', "_").to_uppercase()
    );
    let runner = env::var(&runner_env).ok()?;
    if runner.trim().is_empty() {
        return None;
    }

    let words = split_shell_words(&runner).unwrap_or_else(|error| {
        panic!("{runner_env} contains an invalid runner command ({error})")
    });
    if words.is_empty() { None } else { Some(words) }
}

fn split_shell_words(input: &str) -> Result<Vec<String>, &'static str> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Normal,
        SingleQuoted,
        DoubleQuoted,
    }

    let mut state = State::Normal;
    let mut current = String::new();
    let mut parts = Vec::new();
    let mut pending_empty = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match state {
            State::Normal => match ch {
                c if c.is_whitespace() => {
                    if pending_empty || !current.is_empty() {
                        parts.push(std::mem::take(&mut current));
                        pending_empty = false;
                    }
                }
                '\\' => {
                    let Some(escaped) = chars.next() else {
                        return Err("trailing backslash");
                    };
                    current.push(escaped);
                    pending_empty = false;
                }
                '\'' => {
                    state = State::SingleQuoted;
                }
                '"' => {
                    state = State::DoubleQuoted;
                }
                _ => {
                    current.push(ch);
                    pending_empty = false;
                }
            },
            State::SingleQuoted => {
                if ch == '\'' {
                    if current.is_empty() {
                        pending_empty = true;
                    }
                    state = State::Normal;
                } else {
                    current.push(ch);
                    pending_empty = false;
                }
            }
            State::DoubleQuoted => match ch {
                '"' => {
                    if current.is_empty() {
                        pending_empty = true;
                    }
                    state = State::Normal;
                }
                '\\' => {
                    let Some(escaped) = chars.next() else {
                        return Err("unterminated escape in double quotes");
                    };
                    match escaped {
                        '"' | '\\' | '$' | '`' => current.push(escaped),
                        other => {
                            current.push('\\');
                            current.push(other);
                        }
                    }
                    pending_empty = false;
                }
                _ => {
                    current.push(ch);
                    pending_empty = false;
                }
            },
        }
    }

    match state {
        State::Normal => {
            if pending_empty || !current.is_empty() {
                parts.push(current);
            }
            Ok(parts)
        }
        State::SingleQuoted => Err("unterminated single quote"),
        State::DoubleQuoted => Err("unterminated double quote"),
    }
}

#[cfg(test)]
mod locate_binary_tests {
    use super::locate_binary;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_binary_from_cargo_env_var() {
        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");

        let temp_dir = TempDir::create().expect("failed to create temporary directory");
        let binary_name = "rsync_locate_binary_env";
        let binary_path = temp_dir
            .path()
            .join(format!("{binary_name}{}", std::env::consts::EXE_SUFFIX));
        fs::write(&binary_path, b"test binary").expect("failed to write binary placeholder");

        let env_key = format!("CARGO_BIN_EXE_{binary_name}");
        let _env_guard = EnvVarGuard::set(env_key, OsString::from(&binary_path));

        let resolved = locate_binary(binary_name)
            .unwrap_or_else(|| panic!("expected {binary_name} to be resolved from env var"));
        assert_eq!(resolved, binary_path);
    }

    #[test]
    fn env_guard_restores_absent_variable() {
        const KEY: &str = "_RSYNC_TEST_ENV_GUARD_REMOVED";

        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");

        unsafe {
            env::remove_var(KEY);
        }

        {
            let _env_guard = EnvVarGuard::set(KEY.to_string(), OsString::from("temporary"));
            assert_eq!(env::var_os(KEY), Some(OsString::from("temporary")));
        }

        assert!(
            env::var_os(KEY).is_none(),
            "environment variable should be cleared"
        );
    }

    struct EnvVarGuard {
        key: String,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        #[allow(unsafe_code)]
        fn set<K>(key: K, value: OsString) -> Self
        where
            K: Into<String>,
        {
            let key_string = key.into();
            let original = env::var_os(&key_string);
            // We guard environment mutations with a process-wide mutex to avoid
            // concurrent changes across tests, matching the guidance in
            // `std::env` documentation for multi-threaded programs.
            unsafe {
                env::set_var(&key_string, &value);
            }
            Self {
                key: key_string,
                original,
            }
        }
    }

    impl Drop for EnvVarGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            if let Some(original) = self.original.as_ref() {
                // The global mutex ensures no other thread performs an
                // environment mutation while we restore the prior value.
                unsafe {
                    env::set_var(&self.key, original);
                }
            } else {
                // Protected by the same mutex as other mutations.
                unsafe {
                    env::remove_var(&self.key);
                }
            }
        }
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn create() -> io::Result<Self> {
            let mut base = env::temp_dir();
            base.push("rsync_locate_binary_tests");
            fs::create_dir_all(&base)?;

            for attempt in 0..100 {
                let candidate = base.join(unique_component(attempt));
                match fs::create_dir(&candidate) {
                    Ok(()) => return Ok(Self { path: candidate }),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }

            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "failed to allocate unique temporary directory",
            ))
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn unique_component(attempt: u32) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("pid{}_{}_attempt{}", std::process::id(), timestamp, attempt)
    }
}

#[cfg(test)]
mod program_name_tests {
    use super::unique_program_names;

    #[test]
    fn deduplicates_and_preserves_input_order() {
        let names = ["rsync", "rsync", "oc-rsync", "rsync", "oc-rsyncd"];
        let unique = unique_program_names(&names);

        assert_eq!(unique, vec!["rsync", "oc-rsync", "oc-rsyncd"]);
    }
}

#[cfg(test)]
mod split_shell_words_tests {
    use super::split_shell_words;

    #[test]
    fn splits_whitespace_separated_words() {
        assert_eq!(
            split_shell_words("qemu-aarch64 -L /usr/aarch64-linux-gnu").unwrap(),
            vec![
                String::from("qemu-aarch64"),
                String::from("-L"),
                String::from("/usr/aarch64-linux-gnu"),
            ]
        );
    }

    #[test]
    fn honours_quoted_sections() {
        assert_eq!(
            split_shell_words("\"/opt/Runner Tool/bin/runner\" --flag 'value with spaces'")
                .unwrap(),
            vec![
                String::from("/opt/Runner Tool/bin/runner"),
                String::from("--flag"),
                String::from("value with spaces"),
            ]
        );
    }

    #[test]
    fn honours_backslash_escapes_outside_quotes() {
        assert_eq!(
            split_shell_words("/path/with\\ space arg").unwrap(),
            vec![String::from("/path/with space"), String::from("arg"),]
        );
    }

    #[test]
    fn honours_backslash_escapes_inside_double_quotes() {
        assert_eq!(
            split_shell_words("cmd \"escaped\\\"quote\"").unwrap(),
            vec![String::from("cmd"), String::from("escaped\"quote")]
        );
    }

    #[test]
    fn preserves_empty_argument_from_double_quotes() {
        assert_eq!(
            split_shell_words("binary \"\" tail").unwrap(),
            vec![String::from("binary"), String::new(), String::from("tail"),]
        );
    }

    #[test]
    fn preserves_empty_argument_from_single_quotes() {
        assert_eq!(
            split_shell_words("tool '' next").unwrap(),
            vec![String::from("tool"), String::new(), String::from("next"),]
        );
    }

    #[test]
    fn detects_unterminated_single_quotes() {
        assert!(split_shell_words("cmd 'unterminated").is_err());
    }

    #[test]
    fn detects_unterminated_double_quotes() {
        assert!(split_shell_words("cmd \"unterminated").is_err());
    }

    #[test]
    fn detects_trailing_backslash() {
        let input = format!("cmd {}", '\\');
        assert!(split_shell_words(&input).is_err());
    }
}
