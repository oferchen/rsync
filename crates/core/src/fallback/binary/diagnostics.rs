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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn describe_missing_fallback_binary_no_env_vars() {
        let result = describe_missing_fallback_binary(OsStr::new("rsync"), &[]);
        assert!(result.contains("rsync"));
        assert!(result.contains("set an override environment variable"));
    }

    #[test]
    fn describe_missing_fallback_binary_one_env_var() {
        let result = describe_missing_fallback_binary(OsStr::new("rsync"), &["RSYNC_PATH"]);
        assert!(result.contains("rsync"));
        assert!(result.contains("set RSYNC_PATH to an explicit path"));
    }

    #[test]
    fn describe_missing_fallback_binary_two_env_vars() {
        let result = describe_missing_fallback_binary(
            OsStr::new("rsync"),
            &["RSYNC_PATH", "OC_RSYNC_FALLBACK"],
        );
        assert!(result.contains("set RSYNC_PATH or OC_RSYNC_FALLBACK"));
    }

    #[test]
    fn describe_missing_fallback_binary_three_env_vars() {
        let result = describe_missing_fallback_binary(
            OsStr::new("rsync"),
            &["VAR1", "VAR2", "VAR3"],
        );
        assert!(result.contains("set VAR1, VAR2, or VAR3"));
    }

    #[test]
    fn describe_missing_fallback_binary_four_env_vars() {
        let result = describe_missing_fallback_binary(
            OsStr::new("rsync"),
            &["A", "B", "C", "D"],
        );
        assert!(result.contains("set A, B, C, or D"));
    }

    #[test]
    fn describe_missing_fallback_binary_includes_fallback_not_available() {
        let result = describe_missing_fallback_binary(OsStr::new("rsync"), &[]);
        assert!(result.contains("is not available on PATH"));
        assert!(result.contains("is not executable"));
    }

    #[test]
    fn describe_missing_fallback_binary_includes_install_suggestion() {
        let result = describe_missing_fallback_binary(OsStr::new("rsync"), &[]);
        assert!(result.contains("install upstream rsync"));
    }

    #[test]
    fn describe_missing_fallback_binary_displays_binary_path() {
        let result = describe_missing_fallback_binary(OsStr::new("/usr/bin/rsync"), &[]);
        assert!(result.contains("/usr/bin/rsync"));
    }
}
