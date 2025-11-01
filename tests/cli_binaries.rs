use rsync_core::version::{
    DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME,
};
use std::env;
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
    let binary = binary_path(name);
    if !binary.is_file() {
        panic!("failed to locate {name}: {}", binary.display());
    }

    if let Some(mut runner) = cargo_target_runner() {
        let runner_binary = runner
            .get(0)
            .cloned()
            .unwrap_or_else(|| panic!("{name} runner command is empty"));
        runner.remove(0);
        let mut command = Command::new(runner_binary);
        command.args(runner);
        command.arg(&binary);
        command
    } else {
        Command::new(binary)
    }
}

fn binary_path(name: &str) -> PathBuf {
    let env_var = format!("CARGO_BIN_EXE_{name}");
    if let Some(path) = env::var_os(&env_var) {
        return PathBuf::from(path);
    }

    let mut target_dir = env::current_exe().expect("current_exe should be available");
    target_dir.pop();
    if target_dir.ends_with("deps") {
        target_dir.pop();
    }
    target_dir.push(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    target_dir
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
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match state {
            State::Normal => match ch {
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        parts.push(std::mem::take(&mut current));
                    }
                }
                '\\' => {
                    let Some(escaped) = chars.next() else {
                        return Err("trailing backslash");
                    };
                    current.push(escaped);
                }
                '\'' => {
                    state = State::SingleQuoted;
                }
                '"' => {
                    state = State::DoubleQuoted;
                }
                _ => current.push(ch),
            },
            State::SingleQuoted => {
                if ch == '\'' {
                    state = State::Normal;
                } else {
                    current.push(ch);
                }
            }
            State::DoubleQuoted => match ch {
                '"' => state = State::Normal,
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
                }
                _ => current.push(ch),
            },
        }
    }

    match state {
        State::Normal => {
            if !current.is_empty() {
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
    fn detects_unterminated_single_quotes() {
        assert!(matches!(split_shell_words("cmd 'unterminated"), Err(_)));
    }

    #[test]
    fn detects_unterminated_double_quotes() {
        assert!(matches!(split_shell_words("cmd \"unterminated"), Err(_)));
    }

    #[test]
    fn detects_trailing_backslash() {
        let input = format!("cmd {}", '\\');
        assert!(matches!(split_shell_words(&input), Err(_)));
    }
}
