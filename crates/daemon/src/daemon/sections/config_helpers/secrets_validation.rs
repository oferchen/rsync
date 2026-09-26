/// Validates a secrets file path from an environment variable.
///
/// Returns `Ok(None)` if the file doesn't exist, and includes the environment
/// variable name in error messages.
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

/// Ensures a configured secrets file is usable as one.
///
/// Only the file *type* is checked here. Mode and ownership are NOT: upstream
/// stores the `secrets file` value verbatim at parse time and applies the
/// permission rules in `check_secret()` (authenticate.c:168-181), gated on
/// `lp_strict_modes(module)`. Enforcing them here would make `strict modes = no`
/// unhonourable - the daemon would refuse the whole config instead of serving
/// the other-accessible file the operator deliberately opted into. The single
/// owner of that rule is `platform::secrets::check_secrets_file_permissions`,
/// called from `verify_secret_response` behind the module's `strict_modes` flag.
fn ensure_secrets_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(format!(
            "secrets file '{}' must be a regular file",
            path.display()
        ));
    }

    Ok(())
}
