fn parse_daemon_option(payload: &str) -> Option<&str> {
    let (keyword, remainder) = payload.split_once(char::is_whitespace)?;
    if !keyword.eq_ignore_ascii_case("OPTION") {
        return None;
    }

    let option = remainder.trim();
    if option.is_empty() {
        None
    } else {
        Some(option)
    }
}

fn refused_option<'a>(module: &ModuleDefinition, options: &'a [String]) -> Option<&'a str> {
    options.iter().find_map(|candidate| {
        let canonical_candidate = canonical_option(candidate);
        module
            .refuse_options
            .iter()
            .map(String::as_str)
            .any(|refused| canonical_option(refused) == canonical_candidate)
            .then_some(candidate.as_str())
    })
}

fn canonical_option(text: &str) -> String {
    let token = text
        .trim()
        .trim_start_matches('-')
        .split([' ', '\t', '='])
        .next()
        .unwrap_or("");
    token.to_ascii_lowercase()
}

fn apply_module_timeout(stream: &TcpStream, module: &ModuleDefinition) -> io::Result<()> {
    if let Some(timeout) = module.timeout {
        let duration = Duration::from_secs(timeout.get());
        stream.set_read_timeout(Some(duration))?;
        stream.set_write_timeout(Some(duration))?;
    }

    Ok(())
}

fn missing_argument_value(option: &str) -> DaemonError {
    config_error(format!("missing value for {option}"))
}

fn parse_port(value: &OsString) -> Result<u16, DaemonError> {
    let text = value.to_string_lossy();
    text.parse::<u16>()
        .map_err(|_| config_error(format!("invalid value for --port: '{text}'")))
}

fn parse_bind_address(value: &OsString) -> Result<IpAddr, DaemonError> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();
    let candidate = trimmed
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(trimmed);

    if let Ok(address) = candidate.parse::<IpAddr>() {
        return Ok(address);
    }

    lookup_host(candidate)
        .map_err(|_| config_error(format!("invalid bind address '{text}'")))?
        .into_iter()
        .next()
        .ok_or_else(|| config_error(format!("invalid bind address '{text}'")))
}

fn parse_max_sessions(value: &OsString) -> Result<NonZeroUsize, DaemonError> {
    let text = value.to_string_lossy();
    let parsed: usize = text
        .parse()
        .map_err(|_| config_error(format!("invalid value for --max-sessions: '{text}'")))?;
    NonZeroUsize::new(parsed)
        .ok_or_else(|| config_error("--max-sessions must be greater than zero".to_string()))
}

fn parse_module_definition(
    value: &OsString,
    default_secrets: Option<&Path>,
    default_incoming_chmod: Option<&str>,
    default_outgoing_chmod: Option<&str>,
) -> Result<ModuleDefinition, DaemonError> {
    let text = value.to_string_lossy();
    let (name_part, remainder) = text.split_once('=').ok_or_else(|| {
        config_error(format!(
            "invalid module specification '{text}': expected NAME=PATH"
        ))
    })?;

    let name = name_part.trim();
    ensure_valid_module_name(name).map_err(|msg| config_error(msg.to_string()))?;

    let (path_part, comment_part, options_part) = split_module_path_comment_and_options(remainder);

    let path_text = path_part.trim();
    if path_text.is_empty() {
        return Err(config_error("module path must be non-empty".to_string()));
    }

    let path_text = unescape_module_component(path_text);
    let comment = comment_part
        .map(|value| unescape_module_component(value.trim()))
        .filter(|value| !value.is_empty());

    let mut module = ModuleDefinition {
        name: name.to_string(),
        path: PathBuf::from(&path_text),
        comment,
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
        auth_users: Vec::new(),
        secrets_file: None,
        bandwidth_limit: None,
        bandwidth_limit_specified: false,
        bandwidth_burst: None,
        bandwidth_burst_specified: false,
        bandwidth_limit_configured: false,
        refuse_options: Vec::new(),
        read_only: true,
        write_only: false,
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
        incoming_chmod: None,
        outgoing_chmod: None,
    };

    if let Some(options_text) = options_part {
        apply_inline_module_options(options_text, &mut module)?;
    }

    if module.use_chroot && !module.path.is_absolute() {
        return Err(config_error(format!(
            "module path '{path_text}' must be absolute when 'use chroot' is enabled"
        )));
    }

    if module.auth_users.is_empty() {
        if module.secrets_file.is_none() {
            if let Some(path) = default_secrets {
                module.secrets_file = Some(path.to_path_buf());
            }
        }
        if module.incoming_chmod.is_none() {
            module.incoming_chmod = default_incoming_chmod.map(str::to_string);
        }
        if module.outgoing_chmod.is_none() {
            module.outgoing_chmod = default_outgoing_chmod.map(str::to_string);
        }
        return Ok(module);
    }

    if module.secrets_file.is_none() {
        if let Some(path) = default_secrets {
            module.secrets_file = Some(path.to_path_buf());
        } else {
            return Err(config_error(
                "module specified 'auth users' but did not supply a secrets file".to_string(),
            ));
        }
    }

    if module.incoming_chmod.is_none() {
        module.incoming_chmod = default_incoming_chmod.map(str::to_string);
    }
    if module.outgoing_chmod.is_none() {
        module.outgoing_chmod = default_outgoing_chmod.map(str::to_string);
    }

    Ok(module)
}

fn split_module_path_comment_and_options(value: &str) -> (&str, Option<&str>, Option<&str>) {
    enum Segment {
        Path,
        Comment { start: usize },
    }

    let mut state = Segment::Path;
    let mut escape = false;

    for (idx, ch) in value.char_indices() {
        if escape {
            escape = false;
            continue;
        }

        match ch {
            '\\' => {
                escape = true;
            }
            ';' => {
                let options = value.get(idx + ch.len_utf8()..);
                return match state {
                    Segment::Path => {
                        let path = &value[..idx];
                        (path, None, options)
                    }
                    Segment::Comment { start } => {
                        let comment = value.get(start..idx);
                        let path = &value[..start - 1];
                        (path, comment, options)
                    }
                };
            }
            ',' => {
                if matches!(state, Segment::Path) {
                    state = Segment::Comment {
                        start: idx + ch.len_utf8(),
                    };
                }
            }
            _ => {}
        }
    }

    match state {
        Segment::Path => (value, None, None),
        Segment::Comment { start } => (&value[..start - 1], value.get(start..), None),
    }
}

fn split_inline_options(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut escape = false;

    for ch in text.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' => escape = true,
            ';' => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        parts.push(current.trim().to_string());
    }

    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn apply_inline_module_options(
    options: &str,
    module: &mut ModuleDefinition,
) -> Result<(), DaemonError> {
    let path = Path::new("--module");
    let mut seen = HashSet::new();

    for option in split_inline_options(options) {
        let (key_raw, value_raw) = option
            .split_once('=')
            .ok_or_else(|| config_error(format!("module option '{option}' is missing '='")))?;

        let key = key_raw.trim().to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err(config_error(format!("duplicate module option '{key_raw}'")));
        }

        let value = value_raw.trim();
        match key.as_str() {
            "read only" | "read-only" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'read only'"))
                })?;
                module.read_only = parsed;
            }
            "write only" | "write-only" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'write only'"))
                })?;
                module.write_only = parsed;
            }
            "list" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'list'"))
                })?;
                module.listable = parsed;
            }
            "numeric ids" | "numeric-ids" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'numeric ids'"))
                })?;
                module.numeric_ids = parsed;
            }
            "use chroot" | "use-chroot" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'use chroot'"))
                })?;
                module.use_chroot = parsed;
            }
            "hosts allow" | "hosts-allow" => {
                let patterns = parse_host_list(value, path, 0, "hosts allow")?;
                module.hosts_allow = patterns;
            }
            "hosts deny" | "hosts-deny" => {
                let patterns = parse_host_list(value, path, 0, "hosts deny")?;
                module.hosts_deny = patterns;
            }
            "auth users" | "auth-users" => {
                let users = parse_auth_user_list(value).map_err(|error| {
                    config_error(format!("invalid 'auth users' directive: {error}"))
                })?;
                if users.is_empty() {
                    return Err(config_error(
                        "'auth users' option must list at least one user".to_string(),
                    ));
                }
                module.auth_users = users;
            }
            "secrets file" | "secrets-file" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'secrets file' option must not be empty".to_string(),
                    ));
                }
                module.secrets_file = Some(PathBuf::from(unescape_module_component(value)));
            }
            "bwlimit" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'bwlimit' option must not be empty".to_string(),
                    ));
                }
                let components = parse_runtime_bwlimit(&OsString::from(value))?;
                module.bandwidth_limit = components.rate();
                module.bandwidth_burst = components.burst();
                module.bandwidth_burst_specified = components.burst_specified();
                module.bandwidth_limit_specified = true;
                module.bandwidth_limit_configured = true;
            }
            "refuse options" | "refuse-options" => {
                let options = parse_refuse_option_list(value).map_err(|error| {
                    config_error(format!("invalid 'refuse options' directive: {error}"))
                })?;
                module.refuse_options = options;
            }
            "uid" => {
                let uid = parse_numeric_identifier(value)
                    .ok_or_else(|| config_error(format!("invalid uid '{value}'")))?;
                module.uid = Some(uid);
            }
            "gid" => {
                let gid = parse_numeric_identifier(value)
                    .ok_or_else(|| config_error(format!("invalid gid '{value}'")))?;
                module.gid = Some(gid);
            }
            "timeout" => {
                let timeout = parse_timeout_seconds(value)
                    .ok_or_else(|| config_error(format!("invalid timeout '{value}'")))?;
                module.timeout = timeout;
            }
            "max connections" | "max-connections" => {
                let max = parse_max_connections_directive(value).ok_or_else(|| {
                    config_error(format!("invalid max connections value '{value}'"))
                })?;
                module.max_connections = max;
            }
            "incoming chmod" | "incoming-chmod" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'incoming chmod' option must not be empty".to_string(),
                    ));
                }
                module.incoming_chmod = Some(value.to_string());
            }
            "outgoing chmod" | "outgoing-chmod" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'outgoing chmod' option must not be empty".to_string(),
                    ));
                }
                module.outgoing_chmod = Some(value.to_string());
            }
            _ => {
                return Err(config_error(format!(
                    "unsupported module option '{key_raw}'"
                )));
            }
        }
    }

    Ok(())
}

fn unescape_module_component(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                result.push(next);
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

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

fn duplicate_argument(option: &str) -> DaemonError {
    config_error(format!("duplicate daemon argument '{option}'"))
}

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

pub(crate) fn configured_fallback_binary() -> Option<OsString> {
    None
}

#[cfg(test)]
mod tests;
