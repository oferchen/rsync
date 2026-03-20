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
/// Creates a script matching upstream rsync's format: a single command line
/// without a `#!/bin/sh` shebang. The script uses `--read-batch` with a
/// destination placeholder that defaults to the current directory.
///
/// # Upstream Reference
///
/// - `batch.c:255-312`: `write_batch_shell_file()` writes the raw command
///   without a shebang line. The `.sh` file is opened with mode `S_IRUSR |
///   S_IWUSR | S_IXUSR` (0o700).
pub fn generate_script(config: &BatchConfig) -> BatchResult<()> {
    let script_path = config.script_file_path();
    let batch_name = config.batch_file_path().to_string_lossy();
    let mut file = File::create(&script_path).map_err(|e| {
        BatchError::Io(io::Error::new(
            e.kind(),
            format!("Failed to create script file '{script_path}': {e}"),
        ))
    })?;

    // upstream: write_batch_shell_file() writes the command without a shebang
    writeln!(
        file,
        "oc-rsync --read-batch={} ${{1:-.}}",
        shell_quote(&batch_name)
    )?;

    file.flush()?;

    // upstream: batch_sh_fd opened with S_IRUSR | S_IWUSR | S_IXUSR (0o700)
    set_script_permissions(&script_path)?;

    Ok(())
}

/// Generate a shell script for replaying a batch file with full argument preservation.
///
/// Converts `--write-batch` / `--only-write-batch` arguments to `--read-batch`,
/// preserves relevant options, and embeds filter rules if present. The output
/// matches upstream rsync's `batch.c:write_batch_shell_file()` format.
///
/// # Upstream Reference
///
/// - `batch.c:255-312`: `write_batch_shell_file()` elides filename args,
///   converts write-batch to read-batch, and embeds filter rules via heredoc.
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

    // upstream: write_batch_shell_file() starts with the binary name, no shebang
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

    // upstream: write_opt("${1:-", NULL) + write_arg(dest) + "}"
    write!(file, " ${{1:-")?;
    if let Some(dest) = find_destination(original_args) {
        write!(file, "{}", shell_quote(dest))?;
    }
    write!(file, "}}")?;

    // upstream: write_filter_rules() uses heredoc with #E# delimiter
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

    // upstream: batch_sh_fd opened with S_IRUSR | S_IWUSR | S_IXUSR (0o700)
    set_script_permissions(&script_path)?;

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

/// Set script file permissions to match upstream rsync.
///
/// Upstream rsync opens the `.sh` file with `S_IRUSR | S_IWUSR | S_IXUSR`
/// (0o700), granting read/write/execute only to the owner.
#[cfg(unix)]
fn set_script_permissions(path: &str) -> BatchResult<()> {
    use std::fs;
    let permissions = fs::Permissions::from_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_script_permissions(_path: &str) -> BatchResult<()> {
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

        // Read and verify script content -- upstream has no shebang
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            !content.starts_with("#!/bin/sh"),
            "Upstream rsync batch scripts have no shebang"
        );
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
        // upstream: batch_sh_fd opened with S_IRUSR | S_IWUSR | S_IXUSR (0o700)
        assert_eq!(
            permissions.mode() & 0o777,
            0o700,
            "Script permissions should be exactly 0o700"
        );
    }
}
