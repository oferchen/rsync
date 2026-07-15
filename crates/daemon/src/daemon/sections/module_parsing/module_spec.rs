// CLI/inline module definition and daemon-parameter parsing.
//
// Parses `NAME=PATH` module specifications (with inline `;`-delimited options),
// applies client-sent `--dparam` overrides to a `ModuleDefinition`, and parses
// the daemon's scalar CLI argument values (port, bind address, session limits,
// TCP fast-open mode, bwlimit). Mirrors upstream `loadparm.c` /
// `clientserver.c` per-module config handling.

fn apply_module_timeout(stream: &DaemonStream, module: &ModuleDefinition) -> io::Result<()> {
    if let Some(timeout) = module.timeout {
        let duration = Duration::from_secs(timeout.get());
        stream.set_read_timeout(Some(duration))?;
        stream.set_write_timeout(Some(duration))?;
    } else {
        // #503 (design doc section 4.4): when the module sets no `timeout`,
        // clear the leaked 10-second accept-time `SOCKET_TIMEOUT` (armed in
        // listener.rs) so the data phase matches upstream's `io_timeout = 0`
        // default (io.c:179 short-circuits the timeout check when unset).
        // Without this, the accept-time guard persists through the delta phase
        // and fires on a wedged read as `code 23`. This MUST ship with the
        // Approach C drain thread: alone it would turn the deadlock's abort
        // into an indefinite hang; the drain thread guarantees the phase
        // cannot deadlock, so removing the timeout is safe.
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
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
        .next()
        .ok_or_else(|| config_error(format!("invalid bind address '{text}'")))
}

fn parse_max_sessions(value: &OsString) -> Result<NonZeroUsize, DaemonError> {
    let text = value.to_string_lossy();
    let parsed: usize = text
        .parse()
        .map_err(|_| config_error(format!("invalid value for --max-sessions: '{text}'")))?;
    NonZeroUsize::new(parsed)
        .ok_or_else(|| config_error("--max-sessions must be greater than zero".to_owned()))
}

fn parse_max_connections(value: &OsString) -> Result<NonZeroUsize, DaemonError> {
    let text = value.to_string_lossy();
    let parsed: usize = text
        .parse()
        .map_err(|_| config_error(format!("invalid value for --max-connections: '{text}'")))?;
    NonZeroUsize::new(parsed)
        .ok_or_else(|| config_error("--max-connections must be greater than zero".to_owned()))
}

fn parse_tcp_fastopen_mode(value: &OsString, _brand: Brand) -> Result<TcpFastOpenMode, DaemonError> {
    let text = value.to_string_lossy();
    text.parse::<TcpFastOpenMode>()
        .map_err(|error| config_error(error.to_string()))
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
    ensure_valid_module_name(name).map_err(|msg| config_error(msg.to_owned()))?;

    let (path_part, comment_part, options_part) = split_module_path_comment_and_options(remainder);

    let path_text = path_part.trim();
    if path_text.is_empty() {
        return Err(config_error("module path must be non-empty".to_owned()));
    }

    let path_text = unescape_module_component(path_text);
    let comment = comment_part
        .map(|value| unescape_module_component(value.trim()))
        .filter(|value| !value.is_empty());

    let mut module = ModuleDefinition {
        name: name.to_owned(),
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
        fake_super: false,
        munge_symlinks: None,
        max_verbosity: 1,
        ignore_errors: false,
        ignore_nonreadable: false,
        transfer_logging: false,
        log_format: Some("%o %h [%a] %m (%u) %f %l".to_owned()),
        log_file: None,
        dont_compress: None,
        early_exec: None,
        pre_xfer_exec: None,
        post_xfer_exec: None,
        name_converter: None,
        temp_dir: None,
        charset: None,
        forward_lookup: true,
        strict_modes: true,
        exclude_from: None,
        include_from: None,
        open_noatime: false,
        reverse_lookup: true,
        lock_file: None,
        filter: Vec::new(),
        exclude: Vec::new(),
        include: Vec::new(),
    };

    if let Some(options_text) = options_part {
        apply_inline_module_options(options_text, &mut module)?;
    }

    // Windows has no chroot(2), so suppress the absolute-path check there:
    // upstream rsync's `use chroot` semantics simply don't apply on Windows,
    // and rejecting Unix-style absolute paths (e.g. `/srv/docs`, which Windows
    // `Path::is_absolute()` treats as non-absolute because they lack a drive
    // letter) would block every cross-platform daemon config file.
    //
    // The bare root `/` is accepted intentionally: upstream rsync treats it as
    // a legitimate module path under either `use chroot` setting (loadparm.c
    // P_PATH preserves it; clientserver.c serves from it directly when chroot
    // is off and chroot's into it as a no-op when on).
    #[cfg(unix)]
    if module.use_chroot && !module.path.is_absolute() {
        return Err(config_error(format!(
            "module path '{path_text}' must be absolute when 'use chroot' is enabled"
        )));
    }

    if module.auth_users.is_empty() {
        if module.secrets_file.is_none()
            && let Some(path) = default_secrets
        {
            module.secrets_file = Some(path.to_path_buf());
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
                "module specified 'auth users' but did not supply a secrets file".to_owned(),
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
                parts.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        parts.push(current.trim().to_owned());
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
                        "'auth users' option must list at least one user".to_owned(),
                    ));
                }
                module.auth_users = users;
            }
            "secrets file" | "secrets-file" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'secrets file' option must not be empty".to_owned(),
                    ));
                }
                module.secrets_file = Some(PathBuf::from(unescape_module_component(value)));
            }
            "bwlimit" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'bwlimit' option must not be empty".to_owned(),
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
                        "'incoming chmod' option must not be empty".to_owned(),
                    ));
                }
                let mut owned = String::new();
                value.clone_into(&mut owned);
                module.incoming_chmod = Some(owned);
            }
            "outgoing chmod" | "outgoing-chmod" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'outgoing chmod' option must not be empty".to_owned(),
                    ));
                }
                let mut owned = String::new();
                value.clone_into(&mut owned);
                module.outgoing_chmod = Some(owned);
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

/// Applies client-sent daemon parameter overrides to a module definition.
///
/// Each entry in `params` is a `key=value` string sent by the client via
/// `--dparam` / `-M`. This mirrors upstream rsync's per-session module
/// config overrides (loadparm.c / clientserver.c).
///
/// Only a safe subset of module directives are overridable via dparam:
/// directives that affect access control (hosts allow/deny, auth users,
/// secrets file) are excluded to prevent privilege escalation.
fn apply_daemon_param_overrides(
    params: &[String],
    module: &mut ModuleDefinition,
) -> Result<(), DaemonError> {
    for param in params {
        let (key_raw, value_raw) = param
            .split_once('=')
            .ok_or_else(|| config_error(format!("daemon param '{param}' is missing '='")))?;

        let key = key_raw.trim().to_ascii_lowercase();
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
                        "'incoming chmod' dparam must not be empty".to_owned(),
                    ));
                }
                module.incoming_chmod = Some(value.to_owned());
            }
            "outgoing chmod" | "outgoing-chmod" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'outgoing chmod' dparam must not be empty".to_owned(),
                    ));
                }
                module.outgoing_chmod = Some(value.to_owned());
            }
            "bwlimit" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'bwlimit' dparam must not be empty".to_owned(),
                    ));
                }
                let components = parse_runtime_bwlimit(&OsString::from(value))?;
                module.bandwidth_limit = components.rate();
                module.bandwidth_burst = components.burst();
                module.bandwidth_burst_specified = components.burst_specified();
                module.bandwidth_limit_specified = true;
                module.bandwidth_limit_configured = true;
            }
            // Security-sensitive directives are not overridable via dparam.
            "hosts allow" | "hosts-allow" | "hosts deny" | "hosts-deny" | "auth users"
            | "auth-users" | "secrets file" | "secrets-file" | "refuse options"
            | "refuse-options" | "uid" | "gid" => {
                return Err(config_error(format!(
                    "daemon param '{key_raw}' cannot be overridden via --dparam"
                )));
            }
            _ => {
                // Silently ignore unrecognised directives, matching upstream
                // rsync's behaviour of skipping unknown per-module params.
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
