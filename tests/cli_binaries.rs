use rsync_core::version::{
    DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME,
};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const CLIENT_BINARIES: &[&str] = &[PROGRAM_NAME, OC_PROGRAM_NAME];
const DAEMON_BINARIES: &[&str] = &[DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME];

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
    for &binary in CLIENT_BINARIES {
        if locate_binary(binary).is_none() {
            if binary == OC_PROGRAM_NAME {
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
        assert!(stdout.contains("Usage:"));
        assert!(stdout.contains(binary));
    }
}

#[test]
fn client_without_operands_shows_usage() {
    for &binary in CLIENT_BINARIES {
        if locate_binary(binary).is_none() {
            if binary == OC_PROGRAM_NAME {
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
        assert!(combined.contains("Usage:"));
    }
}

#[test]
fn daemon_help_lists_usage() {
    for &binary in DAEMON_BINARIES {
        if locate_binary(binary).is_none() {
            if binary == OC_DAEMON_PROGRAM_NAME {
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
        assert!(stdout.contains("Usage:"));
        assert!(stdout.contains(binary));
    }
}

#[test]
fn daemon_rejects_unknown_flag() {
    for &binary in DAEMON_BINARIES {
        if locate_binary(binary).is_none() {
            if binary == OC_DAEMON_PROGRAM_NAME {
                panic!("expected {binary} to be available for testing");
            }
            println!(
                "skipping {binary} compatibility wrapper tests because the binary was not built"
            );
            continue;
        }

        let output = binary_output(binary, &["--definitely-not-a-flag"]);
        assert!(
            !output.status.success(),
            "unexpected flags should return a failure exit status for {binary}"
        );
        let combined = combined_utf8(&output);
        assert!(combined.contains("unknown option"));
    }
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
