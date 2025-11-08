use std::ffi::OsStr;
use std::path::Path;

/// Formats a diagnostic explaining that a fallback executable is unavailable.
#[must_use]
pub fn describe_missing_fallback_binary(binary: &OsStr, env_vars: &[&str]) -> String {
    let display = Path::new(binary).display();
    let directive = match env_vars.len() {
        0 => String::from("set an override environment variable to an explicit path"),
        1 => {
            let var = env_vars[0];
            format!("set {var} to an explicit path")
        }
        2 => {
            let first = env_vars[0];
            let second = env_vars[1];
            format!("set {first} or {second} to an explicit path")
        }
        _ => {
            let (head, tail) = env_vars.split_at(env_vars.len() - 1);
            let mut joined = head.join(", ");
            joined.push_str(", or ");
            joined.push_str(tail[0]);
            format!("set {joined} to an explicit path")
        }
    };

    format!(
        "fallback rsync binary '{display}' is not available on PATH or is not executable; install upstream rsync or {directive}"
    )
}
