/// Resolves a uid string to a numeric uid.
///
/// Accepts either a numeric uid (e.g., "1000") or a username (e.g., "nobody").
/// On Unix, usernames are resolved via `getpwnam_r`. On non-Unix, only numeric
/// IDs are accepted.
#[cfg(unix)]
fn resolve_uid(value: &str) -> Result<u32, String> {
    if let Ok(numeric) = value.parse::<u32>() {
        return Ok(numeric);
    }

    match metadata::id_lookup::lookup_user_by_name(value.as_bytes()) {
        Ok(Some(uid)) => Ok(uid),
        Ok(None) => Err(format!("unknown user '{value}'")),
        Err(error) => Err(format!("failed to look up user '{value}': {error}")),
    }
}

/// Resolves a uid string to a numeric uid.
///
/// On non-Unix platforms, only numeric IDs are accepted.
#[cfg(not(unix))]
fn resolve_uid(value: &str) -> Result<u32, String> {
    value.parse::<u32>().map_err(|_| {
        format!("invalid uid '{value}' (only numeric IDs supported on this platform)")
    })
}

/// Resolves a gid string to a numeric gid.
///
/// Accepts either a numeric gid (e.g., "1000") or a groupname (e.g., "nogroup").
/// On Unix, groupnames are resolved via `getgrnam_r`. On non-Unix, only numeric
/// IDs are accepted.
#[cfg(unix)]
fn resolve_gid(value: &str) -> Result<u32, String> {
    if let Ok(numeric) = value.parse::<u32>() {
        return Ok(numeric);
    }

    match metadata::id_lookup::lookup_group_by_name(value.as_bytes()) {
        Ok(Some(gid)) => Ok(gid),
        Ok(None) => Err(format!("unknown group '{value}'")),
        Err(error) => Err(format!("failed to look up group '{value}': {error}")),
    }
}

/// Resolves a gid string to a numeric gid.
///
/// On non-Unix platforms, only numeric IDs are accepted.
#[cfg(not(unix))]
fn resolve_gid(value: &str) -> Result<u32, String> {
    value.parse::<u32>().map_err(|_| {
        format!("invalid gid '{value}' (only numeric IDs supported on this platform)")
    })
}

fn validate_cli_secrets_file(path: PathBuf) -> Result<PathBuf, DaemonError> {
    let metadata = fs::metadata(&path).map_err(|error| {
        config_error(format!(
            "failed to access secrets file '{}': {}",
            path.display(),
            error
        ))
    })?;

    if let Err(detail) = ensure_secrets_file(&path, &metadata) {
        return Err(config_error(detail));
    }

    Ok(path)
}
