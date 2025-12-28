//! crates/batch/src/script.rs
//!
//! Shell script generation for batch replay.
//!
//! Creates a .sh script that can be used to replay a batch file,
//! matching upstream rsync's format.

use crate::BatchConfig;
use crate::error::{BatchError, BatchResult};
use std::fs::File;
use std::io::{self, Write};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Generate a minimal shell script for replaying a batch file.
///
/// Creates a simple script that uses --read-batch with the destination placeholder.
pub fn generate_script(config: &BatchConfig) -> BatchResult<()> {
    let script_path = config.script_file_path();
    let batch_name = config.batch_file_path().to_string_lossy();
    let mut file = File::create(&script_path).map_err(|e| {
        BatchError::Io(io::Error::new(
            e.kind(),
            format!("Failed to create script file '{script_path}': {e}"),
        ))
    })?;

    // Write a minimal replay script
    writeln!(file, "#!/bin/sh")?;
    writeln!(
        file,
        "oc-rsync --read-batch={} \"${{1:-.}}\"",
        shell_quote(&batch_name)
    )?;

    file.flush()?;

    // Make the script executable
    make_executable(&script_path)?;

    Ok(())
}

/// Generate a shell script for replaying a batch file with full argument preservation.
///
/// The script converts the --write-batch command to a --read-batch command,
/// preserving relevant options.
pub fn generate_script_with_args(
    config: &BatchConfig,
    original_args: &[String],
    filter_rules: Option<&str>,
) -> BatchResult<()> {
    let script_path = config.script_file_path();
    let mut file = File::create(&script_path).map_err(|e| {
        BatchError::Io(io::Error::new(
            e.kind(),
            format!("Failed to create script file '{script_path}': {e}"),
        ))
    })?;

    // Write the shebang and initial command
    writeln!(file, "#!/bin/sh")?;
    write!(file, "{}", original_args[0])?; // rsync binary name

    // Process arguments, converting write-batch to read-batch
    for arg in &original_args[1..] {
        if let Some(batch_name) = arg.strip_prefix("--write-batch=") {
            // Extract batch name and convert to read-batch
            write!(file, " --read-batch={}", shell_quote(batch_name))?;
        } else if let Some(batch_name) = arg.strip_prefix("--only-write-batch=") {
            // Extract batch name and convert to read-batch
            write!(file, " --read-batch={}", shell_quote(batch_name))?;
        } else if arg == "--write-batch" || arg == "--only-write-batch" {
            // Skip these, they'll be followed by a value
            continue;
        } else if arg.starts_with("--files-from")
            || arg.starts_with("--filter")
            || arg.starts_with("--include")
            || arg.starts_with("--exclude")
        {
            // Skip file-based filters, they'll be embedded below
            if !arg.contains('=') {
                // Skip the next argument too (the value)
                continue;
            }
            continue;
        } else if arg == "-f" {
            // Skip filter shortcut
            continue;
        } else {
            // Pass through other arguments
            write!(file, " {}", shell_quote(arg))?;
        }
    }

    // Add the destination placeholder
    write!(file, " \"${{1:-")?;
    // Extract destination from original args (last non-option argument)
    if let Some(dest) = find_destination(original_args) {
        write!(file, "{}", shell_quote(dest))?;
    }
    write!(file, "}}\"")?;

    // Embed filter rules if present
    if let Some(rules) = filter_rules {
        writeln!(file, " <<'#E#'")?;
        write!(file, "{rules}")?;
        if !rules.ends_with('\n') {
            writeln!(file)?;
        }
        write!(file, "#E#")?;
    }

    writeln!(file)?;

    file.flush()?;

    // Make the script executable
    make_executable(&script_path)?;

    Ok(())
}

/// Quote a string for safe shell usage.
fn shell_quote(s: &str) -> String {
    // Check if quoting is needed
    if s.chars().all(|c| {
        c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.' || c == ':' || c == '='
    }) {
        return s.to_owned();
    }

    // Need quoting - use single quotes and escape any single quotes
    let mut result = String::from("'");
    for ch in s.chars() {
        if ch == '\'' {
            result.push_str("'\\''");
        } else {
            result.push(ch);
        }
    }
    result.push('\'');
    result
}

/// Find the destination path from the argument list.
fn find_destination(args: &[String]) -> Option<&str> {
    // The destination is typically the last non-option argument
    args.iter()
        .rev()
        .find(|arg| !arg.starts_with('-') && !arg.is_empty())
        .map(|s| s.as_str())
}

/// Make a file executable on Unix systems.
#[cfg(unix)]
fn make_executable(path: &str) -> BatchResult<()> {
    use std::fs;
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111); // Add execute bits
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &str) -> BatchResult<()> {
    // No-op on non-Unix systems
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BatchMode;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_shell_quote() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote("with-dash"), "with-dash");
        assert_eq!(shell_quote("/path/to/file"), "/path/to/file");
        assert_eq!(shell_quote("needs quoting"), "'needs quoting'");
        assert_eq!(shell_quote("has'quote"), "'has'\\''quote'");
        assert_eq!(shell_quote("$special"), "'$special'");
    }

    #[test]
    fn test_find_destination() {
        let args = vec![
            "rsync".to_owned(),
            "-av".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];
        assert_eq!(find_destination(&args), Some("dest/"));

        let args2 = vec![
            "rsync".to_owned(),
            "--write-batch=batch".to_owned(),
            "-av".to_owned(),
            "src".to_owned(),
        ];
        assert_eq!(find_destination(&args2), Some("src"));
    }

    #[test]
    fn test_generate_script() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let result = generate_script(&config);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        assert!(Path::new(&script_path).exists());

        // Read and verify script content
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(content.starts_with("#!/bin/sh\n"));
        assert!(content.contains("--read-batch="));
        assert!(content.contains("oc-rsync"));
    }

    #[test]
    fn test_generate_script_with_filters() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=test.batch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let filter_rules = "- *.tmp\n+ */\n+ *.txt\n- *\n";

        let result = generate_script_with_args(&config, &args, Some(filter_rules));
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(content.contains("<<'#E#'"));
        assert!(content.contains(filter_rules));
        assert!(content.contains("#E#"));
    }

    #[test]
    #[cfg(unix)]
    fn test_script_is_executable() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        generate_script(&config).unwrap();

        let script_path = config.script_file_path();
        let metadata = fs::metadata(&script_path).unwrap();
        let permissions = metadata.permissions();
        assert!(permissions.mode() & 0o111 != 0); // Has execute bits
    }
}
