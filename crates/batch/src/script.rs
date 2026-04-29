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
/// - `batch.c:258-267`: filter rules use `--filter=._-` (protocol >= 29) or
///   `--exclude-from=-` (protocol < 29) to consume the heredoc from stdin.
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

    // upstream: batch.c:261 write_arg(raw_argv[0]) - binary name, no shebang
    write!(file, "{}", original_args[0])?;

    // upstream: batch.c:262-267 - if filter rules are present, add the option
    // that tells rsync to read them from stdin (the heredoc appended below)
    if filter_rules.is_some() {
        if config.protocol_version >= 29 {
            // upstream: batch.c:263-264 write_opt("--filter", "._-")
            write!(file, " --filter=._-")?;
        } else {
            // upstream: batch.c:265-266 write_opt("--exclude-from", "-")
            write!(file, " --exclude-from=-")?;
        }
    }

    // upstream: batch.c:270-298 - process arguments, skipping filenames and
    // converting write-batch to read-batch. We iterate with an index to
    // handle bare options that consume the following value argument.
    let args = &original_args[1..];
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if let Some(batch_name) = arg.strip_prefix("--write-batch=") {
            // upstream: batch.c:292-294 convert --write-batch to --read-batch
            write!(file, " --read-batch={}", shell_quote(batch_name))?;
        } else if let Some(batch_name) = arg.strip_prefix("--only-write-batch=") {
            // upstream: batch.c:292-294 convert --only-write-batch to --read-batch
            write!(file, " --read-batch={}", shell_quote(batch_name))?;
        } else if arg == "--write-batch" || arg == "--only-write-batch" {
            // upstream: batch.c:292-294 bare form - next arg is the batch name
            i += 1;
            if i < args.len() {
                write!(file, " --read-batch={}", shell_quote(&args[i]))?;
            }
        } else if arg.starts_with("--files-from")
            || arg.starts_with("--filter")
            || arg.starts_with("--include")
            || arg.starts_with("--exclude")
        {
            // upstream: batch.c:280-283 skip filter/include/exclude options
            if !arg.contains('=') {
                i += 1; // skip the following value argument
            }
        } else if arg == "-f" {
            // upstream: batch.c:288-289 skip -f (filter shortcut) + its value
            i += 1;
        } else {
            // upstream: batch.c:296-297 pass through other arguments
            write!(file, " {}", shell_quote(arg))?;
        }

        i += 1;
    }

    // upstream: batch.c:300-304 write destination placeholder
    // write_opt("${1:-", NULL) + write_arg(dest) + "}"
    write!(file, " ${{1:-")?;
    if let Some(dest) = find_destination(original_args) {
        write!(file, "{}", shell_quote(dest))?;
    }
    write!(file, "}}")?;

    // upstream: batch.c:305-306 write_filter_rules() uses heredoc with #E# delimiter
    if let Some(rules) = filter_rules {
        // upstream: batch.c:209 write_sbuf(fd, " <<'#E#'\n")
        writeln!(file, " <<'#E#'")?;
        write!(file, "{rules}")?;
        if !rules.ends_with('\n') {
            writeln!(file)?;
        }
        // upstream: batch.c:221 write_sbuf(fd, "#E#")
        write!(file, "#E#")?;
    }

    writeln!(file)?;

    file.flush()?;

    // upstream: batch.c:232 batch_sh_fd opened with S_IRUSR | S_IWUSR | S_IXUSR (0o700)
    set_script_permissions(&script_path)?;

    Ok(())
}

/// Quote a string for safe shell usage.
///
/// Returns the string unchanged when it only contains shell-safe characters
/// (alphanumerics plus `-_/.:=`); otherwise wraps it in single quotes and
/// escapes embedded single quotes using the `'\''` idiom.
fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| {
        c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.' || c == ':' || c == '='
    }) {
        return s.to_owned();
    }

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
        // upstream: batch.c:263-264 adds --filter=._- for protocol >= 29
        assert!(
            content.contains("--filter=._-"),
            "Script must include --filter=._- for protocol >= 29 to consume heredoc: {content}"
        );
        assert!(content.contains("<<'#E#'"));
        assert!(content.contains(filter_rules));
        assert!(content.contains("#E#"));
    }

    /// Verify that filter rules use --exclude-from=- for protocol < 29.
    ///
    /// upstream: batch.c:265-266 write_opt("--exclude-from", "-")
    #[test]
    fn test_generate_script_with_filters_protocol_28() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            28, // protocol < 29
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=test.batch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let filter_rules = "- *.log\n";

        let result = generate_script_with_args(&config, &args, Some(filter_rules));
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--exclude-from=-"),
            "Script must include --exclude-from=- for protocol < 29: {content}"
        );
        assert!(!content.contains("--filter=._-"));
    }

    /// Verify that bare --write-batch (without =) is handled correctly.
    ///
    /// upstream: batch.c:292-294 handles both --write-batch=NAME and bare forms.
    #[test]
    fn test_generate_script_bare_write_batch() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch".to_owned(),
            "mybatch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let result = generate_script_with_args(&config, &args, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--read-batch=mybatch"),
            "Bare --write-batch should be converted to --read-batch=<name>: {content}"
        );
        // The batch name should not appear as a passthrough argument
        let occurrences = content.matches("mybatch").count();
        assert_eq!(
            occurrences, 1,
            "Batch name should appear exactly once (in --read-batch=): {content}"
        );
    }

    /// Verify that no filter option is added when no filter rules are present.
    #[test]
    fn test_generate_script_no_filters() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=mybatch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let result = generate_script_with_args(&config, &args, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        assert!(
            !content.contains("--filter"),
            "No --filter option without filter rules: {content}"
        );
        assert!(
            !content.contains("--exclude-from"),
            "No --exclude-from without filter rules: {content}"
        );
        assert!(
            !content.contains("#E#"),
            "No heredoc without filter rules: {content}"
        );
    }

    /// Verify that --filter and -f args from original command are stripped.
    ///
    /// upstream: batch.c:280-289 skips --filter, --include, --exclude, -f args.
    #[test]
    fn test_generate_script_strips_filter_args() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--filter=._-".to_owned(),
            "--exclude".to_owned(),
            "*.tmp".to_owned(),
            "-f".to_owned(),
            "+ */".to_owned(),
            "--include=*.txt".to_owned(),
            "--write-batch=mybatch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let result = generate_script_with_args(&config, &args, None);
        assert!(result.is_ok());

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();
        // Original filter args should be stripped
        assert!(
            !content.contains("*.tmp"),
            "Excluded patterns should be stripped: {content}"
        );
        assert!(
            !content.contains("+ */"),
            "Filter rule values should be stripped: {content}"
        );
        assert!(
            !content.contains("--include=*.txt"),
            "Include args should be stripped: {content}"
        );
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
