// Bandwidth-limit parsing and daemon error constructors.
//
// Parses runtime and config-file `bwlimit` values and builds the `DaemonError`
// instances used throughout daemon configuration parsing: config errors,
// parse/IO errors, repeated-CLI-argument errors, module-name validation, and
// network (bind/accept/stream) errors. Exit codes mirror upstream rsync.

fn parse_runtime_bwlimit(value: &OsString) -> Result<BandwidthLimitComponents, DaemonError> {
    let text = value.to_string_lossy();
    match parse_bandwidth_limit(&text) {
        Ok(components) => Ok(components),
        Err(error) => Err(runtime_bwlimit_error(&text, error)),
    }
}

fn parse_config_bwlimit(
    value: &str,
    path: &Path,
    line: usize,
) -> Result<BandwidthLimitComponents, DaemonError> {
    match parse_bandwidth_limit(value) {
        Ok(components) => Ok(components),
        Err(error) => Err(config_bwlimit_error(path, line, value, error)),
    }
}

fn runtime_bwlimit_error(value: &str, error: BandwidthParseError) -> DaemonError {
    let text = match error {
        BandwidthParseError::Invalid => format!("--bwlimit={value} is invalid"),
        BandwidthParseError::TooSmall => {
            format!("--bwlimit={value} is too small (min: 512 or 0 for unlimited)")
        }
        BandwidthParseError::TooLarge => format!("--bwlimit={value} is too large"),
    };
    config_error(text)
}

fn config_bwlimit_error(
    path: &Path,
    line: usize,
    value: &str,
    error: BandwidthParseError,
) -> DaemonError {
    let detail = match error {
        BandwidthParseError::Invalid => format!("invalid 'bwlimit' value '{value}'"),
        BandwidthParseError::TooSmall => {
            format!("'bwlimit' value '{value}' is too small (min: 512 or 0 for unlimited)")
        }
        BandwidthParseError::TooLarge => format!("'bwlimit' value '{value}' is too large"),
    };
    config_parse_error(path, line, detail)
}

fn unsupported_option(option: OsString, brand: Brand) -> DaemonError {
    let option = option.to_string_lossy();
    let program = brand.daemon_program_name();
    let text = format!(
        "unknown option '{option}': run '{program} --help' to review supported daemon flags"
    );
    config_error(text)
}

fn config_error(text: String) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon),
    )
}

fn secrets_env_error(env: &'static str, path: &Path, detail: impl Into<String>) -> DaemonError {
    config_error(format!(
        "environment variable {env} points to invalid secrets file '{}': {}",
        path.display(),
        detail.into()
    ))
}

fn config_parse_error(path: &Path, line: usize, message: impl Into<String>) -> DaemonError {
    let text = format!(
        "failed to parse config '{}': {} (line {})",
        path.display(),
        message.into(),
        line
    );
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon),
    )
}

fn config_io_error(action: &str, path: &Path, error: io::Error) -> DaemonError {
    let text = format!("failed to {action} config '{}': {error}", path.display());
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon),
    )
}

fn ensure_valid_module_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("module name must be non-empty and cannot contain whitespace");
    }

    if name
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '/' || ch == '\\')
    {
        return Err("module name cannot contain whitespace or path separators");
    }

    Ok(())
}

/// Validates an already-collapsed `rsyncd.conf` `[section]` header name.
///
/// Deliberately separate from `ensure_valid_module_name`: the whitespace
/// collapse that makes a single interior space legal lives in
/// params.c:Section(), which upstream runs only while scanning a config-file
/// section header. The daemon's CLI `NAME=PATH` specs never pass through that
/// scanner, so they keep the stricter no-whitespace rule.
///
/// The caller must pass a name produced by `collapse_section_name`, so the only
/// whitespace it can still hold is a single interior space. Path separators
/// stay fatal exactly as they are for a CLI spec.
fn ensure_valid_section_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("module name must be non-empty and cannot contain whitespace");
    }

    if name.chars().any(|ch| ch == '/' || ch == '\\') {
        return Err("module name cannot contain whitespace or path separators");
    }

    Ok(())
}

fn duplicate_argument(option: &str) -> DaemonError {
    config_error(format!("duplicate daemon argument '{option}'"))
}

/// Reports two CLI `--module NAME=PATH` specs claiming the same module name, or
/// a CLI spec colliding with a module the config file already defined.
///
/// This is deliberately not reachable from a config file alone: a repeated
/// `[name]` header re-opens the section the first header created
/// (loadparm.c:add_a_section:320-339), so the two blocks merge into one module.
/// `--module` has no upstream equivalent, and two specs for one name have no
/// merge order to follow, so the collision stays an error there.
fn duplicate_module(name: &str) -> DaemonError {
    config_error(format!("duplicate module definition '{name}'"))
}

fn bind_error(address: SocketAddr, error: io::Error) -> DaemonError {
    network_error("bind listener", address, error)
}

fn accept_error(address: SocketAddr, error: io::Error) -> DaemonError {
    network_error("accept connection on", address, error)
}

fn stream_error(peer: Option<SocketAddr>, action: &str, error: io::Error) -> DaemonError {
    match peer {
        Some(addr) => network_error(action, addr, error),
        None => network_error(action, "connection", error),
    }
}

fn network_error<T: fmt::Display>(action: &str, target: T, error: io::Error) -> DaemonError {
    let text = format!("failed to {action} {target}: {error}");
    DaemonError::new(
        SOCKET_IO_EXIT_CODE,
        rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon),
    )
}
