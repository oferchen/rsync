use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::sync::Mutex;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn oc_rsync_version_matches_upstream_golden() {
    let _guard = ENV_MUTEX.lock().expect("environment mutex poisoned");
    let _umask_guard = UmaskGuard::new(0o022);

    let scenario = GoldenScenario::new("cli_version");
    scenario.assert_matches(&["--version"]);
}

struct GoldenScenario {
    name: &'static str,
    path: PathBuf,
}

impl GoldenScenario {
    fn new(name: &'static str) -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = manifest_dir
            .join("tests")
            .join("goldens")
            .join("upstream")
            .join(name);
        if !path.is_dir() {
            panic!("golden scenario '{name}' is missing at {}", path.display());
        }
        Self { name, path }
    }

    fn assert_matches(&self, args: &[&str]) {
        let mut command = oc_rsync_command();
        command.args(args);
        command.env("LC_ALL", "C");
        command.env("TZ", "UTC");
        command.env("COLUMNS", "80");
        command.env("RSYNC_TEST_FIXED_TIME", "1700000000");
        command.env("UMASK", "022");

        let output = command.output().expect("failed to execute oc-rsync");
        let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
        let stderr = String::from_utf8(output.stderr).expect("stderr must be valid UTF-8");
        let normalized_stdout = normalize_dynamic_fields(&stdout);
        let normalized_stderr = normalize_dynamic_fields(&stderr);

        let expected_stdout = self.read_text("stdout.txt");
        let expected_stderr = self.read_text("stderr.txt");
        let expected_exit = self.read_exit_code();
        let actual_exit = output
            .status
            .code()
            .unwrap_or_else(|| panic!("oc-rsync terminated by signal in scenario '{}'", self.name));

        assert_eq!(
            normalized_stdout, expected_stdout,
            "stdout mismatch for scenario '{}'",
            self.name
        );
        assert_eq!(
            normalized_stderr, expected_stderr,
            "stderr mismatch for scenario '{}'",
            self.name
        );
        assert_eq!(
            actual_exit, expected_exit,
            "exit code mismatch for scenario '{}'",
            self.name
        );
    }

    fn read_text(&self, file: &str) -> String {
        let path = self.path.join(file);
        fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {} for scenario '{}': {}",
                path.display(),
                self.name,
                error
            )
        })
    }

    fn read_exit_code(&self) -> i32 {
        let text = self.read_text("exit_code.txt");
        text.trim().parse::<i32>().unwrap_or_else(|error| {
            panic!(
                "invalid exit code in scenario '{}': {} (parsed from {:?})",
                self.name, error, text
            )
        })
    }
}

fn normalize_dynamic_fields(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '#' {
            normalized.push('#');
            let mut consumed = false;
            while let Some(&next) = chars.peek() {
                if next.is_ascii_hexdigit() {
                    consumed = true;
                    chars.next();
                } else {
                    break;
                }
            }
            if consumed {
                normalized.push_str("<hash>");
            }
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

struct UmaskGuard {
    #[cfg(unix)]
    previous: libc::mode_t,
}

impl UmaskGuard {
    fn new(mode: u32) -> Self {
        #[cfg(unix)]
        {
            let previous = unsafe { libc::umask(mode as libc::mode_t) };
            Self { previous }
        }

        #[cfg(not(unix))]
        {
            let _ = mode;
            Self {}
        }
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::umask(self.previous);
        }
    }
}

fn oc_rsync_command() -> StdCommand {
    let binary = locate_binary("oc-rsync")
        .unwrap_or_else(|| panic!("failed to locate oc-rsync binary for golden scenario"));

    if let Some(runner) = cargo_target_runner() {
        let mut iter = runner.into_iter();
        let runner_binary = iter
            .next()
            .unwrap_or_else(|| panic!("CARGO_TARGET runner command is empty"));
        let mut command = StdCommand::new(runner_binary);
        command.args(iter);
        command.arg(binary);
        command
    } else {
        StdCommand::new(binary)
    }
}

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
                    if let Ok(file_type) = entry.file_type() {
                        if file_type.is_dir() {
                            let entry_path = entry.path();
                            candidates.insert(entry_path.join(&binary_name));
                            candidates.insert(entry_path.join("debug").join(&binary_name));
                            candidates.insert(entry_path.join("release").join(&binary_name));
                        }
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

    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(&binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

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
