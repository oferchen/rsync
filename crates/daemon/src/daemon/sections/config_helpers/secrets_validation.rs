/// Validates a secrets file path from a config directive.
///
/// Checks that the file exists, is a regular file, and has secure permissions
/// (0600 on Unix). Returns a [`DaemonError`] with config context on failure.
fn validate_secrets_file(
    path: &Path,
    config_path: &Path,
    line: usize,
) -> Result<PathBuf, DaemonError> {
    let metadata = fs::metadata(path).map_err(|error| {
        config_parse_error(
            config_path,
            line,
            format!(
                "failed to access secrets file '{}': {}",
                path.display(),
                error
            ),
        )
    })?;

    if let Err(detail) = ensure_secrets_file(path, &metadata) {
        return Err(config_parse_error(config_path, line, detail));
    }

    Ok(path.to_path_buf())
}

/// Validates a secrets file path from an environment variable.
///
/// Similar to [`validate_secrets_file`], but returns `Ok(None)` if the file
/// doesn't exist, and includes the environment variable name in error messages.
fn validate_secrets_file_from_env(
    path: &Path,
    env: &'static str,
) -> Result<Option<PathBuf>, DaemonError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }

            return Err(secrets_env_error(
                env,
                path,
                format!("could not be accessed: {error}"),
            ));
        }
    };

    if let Err(detail) = ensure_secrets_file(path, &metadata) {
        return Err(secrets_env_error(env, path, detail));
    }

    Ok(Some(path.to_path_buf()))
}

/// Ensures a file has proper secrets file permissions.
///
/// Verifies the file is a regular file and (on Unix) has mode 0600.
fn ensure_secrets_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(format!(
            "secrets file '{}' must be a regular file",
            path.display()
        ));
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "secrets file '{}' must not be accessible to group or others (expected permissions 0600)",
                path.display()
            ));
        }
    }

    Ok(())
}
