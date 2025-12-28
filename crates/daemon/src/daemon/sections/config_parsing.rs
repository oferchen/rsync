#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfigDirectiveOrigin {
    path: PathBuf,
    line: usize,
}

#[derive(Debug)]
pub(crate) struct ParsedConfigModules {
    modules: Vec<ModuleDefinition>,
    global_refuse_options: Vec<(Vec<String>, ConfigDirectiveOrigin)>,
    motd_lines: Vec<String>,
    pid_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    reverse_lookup: Option<(bool, ConfigDirectiveOrigin)>,
    lock_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    global_bandwidth_limit: Option<(BandwidthLimitComponents, ConfigDirectiveOrigin)>,
    global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    global_incoming_chmod: Option<(String, ConfigDirectiveOrigin)>,
    global_outgoing_chmod: Option<(String, ConfigDirectiveOrigin)>,
}

pub(crate) fn parse_config_modules(path: &Path) -> Result<ParsedConfigModules, DaemonError> {
    let mut stack = Vec::new();
    parse_config_modules_inner(path, &mut stack)
}

fn parse_config_modules_inner(
    path: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<ParsedConfigModules, DaemonError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| config_io_error("read", path, error))?;

    if stack.iter().any(|seen| seen == &canonical) {
        return Err(config_parse_error(
            path,
            0,
            format!("recursive include detected for '{}'", canonical.display()),
        ));
    }

    let contents = fs::read_to_string(&canonical)
        .map_err(|error| config_io_error("read", &canonical, error))?;
    stack.push(canonical.clone());

    let mut modules = Vec::new();
    let mut current: Option<ModuleDefinitionBuilder> = None;
    let mut global_refuse_directives = Vec::new();
    let mut global_refuse_line: Option<usize> = None;
    let mut motd_lines = Vec::new();
    let mut pid_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;
    let mut reverse_lookup: Option<(bool, ConfigDirectiveOrigin)> = None;
    let mut lock_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;
    let mut global_bwlimit: Option<(BandwidthLimitComponents, ConfigDirectiveOrigin)> = None;
    let mut global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;
    let mut global_incoming_chmod: Option<(String, ConfigDirectiveOrigin)> = None;
    let mut global_outgoing_chmod: Option<(String, ConfigDirectiveOrigin)> = None;

    let result = (|| -> Result<ParsedConfigModules, DaemonError> {
        for (index, raw_line) in contents.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim();

            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if line.starts_with('[') {
                let end = line.find(']').ok_or_else(|| {
                    config_parse_error(path, line_number, "unterminated module header")
                })?;
                let name = line[1..end].trim();

                if name.is_empty() {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "module name must be non-empty",
                    ));
                }

                ensure_valid_module_name(name)
                    .map_err(|msg| config_parse_error(path, line_number, msg))?;

                let trailing = line[end + 1..].trim();
                if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';')
                {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "unexpected characters after module header",
                    ));
                }

                if let Some(builder) = current.take() {
                    let default_secrets = global_secrets_file.as_ref().map(|(p, _)| p.as_path());
                    let default_incoming =
                        global_incoming_chmod.as_ref().map(|(value, _)| value.as_str());
                    let default_outgoing =
                        global_outgoing_chmod.as_ref().map(|(value, _)| value.as_str());
                    modules.push(builder.finish(
                        path,
                        default_secrets,
                        default_incoming,
                        default_outgoing,
                    )?);
                }

                current = Some(ModuleDefinitionBuilder::new(name.to_owned(), line_number));
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                config_parse_error(path, line_number, "expected 'key = value' directive")
            })?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            if let Some(builder) = current.as_mut() {
                match key.as_str() {
                    "path" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "module path directive must not be empty",
                            ));
                        }
                        builder.set_path(PathBuf::from(value), path, line_number)?;
                    }
                    "comment" => {
                        let comment = if value.is_empty() {
                            None
                        } else {
                            Some(value.to_owned())
                        };
                        builder.set_comment(comment, path, line_number)?;
                    }
                    "hosts allow" => {
                        let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
                        builder.set_hosts_allow(patterns, path, line_number)?;
                    }
                    "hosts deny" => {
                        let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
                        builder.set_hosts_deny(patterns, path, line_number)?;
                    }
                    "auth users" => {
                        let users = parse_auth_user_list(value).map_err(|error| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid 'auth users' directive: {error}"),
                            )
                        })?;
                        builder.set_auth_users(users, path, line_number)?;
                    }
                    "secrets file" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'secrets file' directive must not be empty",
                            ));
                        }
                        builder.set_secrets_file(PathBuf::from(value), path, line_number)?;
                    }
                    "bwlimit" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'bwlimit' directive must not be empty",
                            ));
                        }
                        let components = parse_config_bwlimit(value, path, line_number)?;
                        builder.set_bandwidth_limit(
                            components.rate(),
                            components.burst(),
                            components.burst_specified(),
                            path,
                            line_number,
                        )?;
                    }
                    "refuse options" => {
                        let options = parse_refuse_option_list(value).map_err(|error| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid 'refuse options' directive: {error}"),
                            )
                        })?;
                        builder.set_refuse_options(options, path, line_number)?;
                    }
                    "read only" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'read only'"),
                            )
                        })?;
                        builder.set_read_only(parsed, path, line_number)?;
                    }
                    "write only" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'write only'"),
                            )
                        })?;
                        builder.set_write_only(parsed, path, line_number)?;
                    }
                    "use chroot" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'use chroot'"),
                            )
                        })?;
                        builder.set_use_chroot(parsed, path, line_number)?;
                    }
                    "numeric ids" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'numeric ids'"),
                            )
                        })?;
                        builder.set_numeric_ids(parsed, path, line_number)?;
                    }
                    "list" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'list'"),
                            )
                        })?;
                        builder.set_listable(parsed, path, line_number)?;
                    }
                    "uid" => {
                        let uid = parse_numeric_identifier(value).ok_or_else(|| {
                            config_parse_error(path, line_number, format!("invalid uid '{value}'"))
                        })?;
                        builder.set_uid(uid, path, line_number)?;
                    }
                    "gid" => {
                        let gid = parse_numeric_identifier(value).ok_or_else(|| {
                            config_parse_error(path, line_number, format!("invalid gid '{value}'"))
                        })?;
                        builder.set_gid(gid, path, line_number)?;
                    }
                    "timeout" => {
                        let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid timeout '{value}'"),
                            )
                        })?;
                        builder.set_timeout(timeout, path, line_number)?;
                    }
                    "max connections" => {
                        let max = parse_max_connections_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid max connections value '{value}'"),
                            )
                        })?;
                        builder.set_max_connections(max, path, line_number)?;
                    }
                    "incoming chmod" | "incoming-chmod" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'incoming chmod' directive must not be empty",
                            ));
                        }
                        builder.set_incoming_chmod(
                            Some(value.to_owned()),
                            path,
                            line_number,
                        )?;
                    }
                    "outgoing chmod" | "outgoing-chmod" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'outgoing chmod' directive must not be empty",
                            ));
                        }
                        builder.set_outgoing_chmod(
                            Some(value.to_owned()),
                            path,
                            line_number,
                        )?;
                    }
                    _ => {
                        // Unsupported directives are ignored for now.
                    }
                }
                continue;
            }

            match key.as_str() {
                "refuse options" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'refuse options' directive must not be empty",
                        ));
                    }
                    let options = parse_refuse_option_list(value).map_err(|error| {
                        config_parse_error(
                            path,
                            line_number,
                            format!("invalid 'refuse options' directive: {error}"),
                        )
                    })?;

                    if let Some(existing_line) = global_refuse_line {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            format!(
                                "duplicate 'refuse options' directive in global section (previously defined on line {existing_line})"
                            ),
                        ));
                    }

                    global_refuse_line = Some(line_number);
                    global_refuse_directives.push((
                        options,
                        ConfigDirectiveOrigin {
                            path: canonical.clone(),
                            line: line_number,
                        },
                    ));
                }
                "include" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'include' directive must not be empty",
                        ));
                    }

                    let include_path = resolve_config_relative_path(&canonical, trimmed);
                    let included = parse_config_modules_inner(&include_path, stack)?;

                    if !included.modules.is_empty() {
                        modules.extend(included.modules);
                    }

                    if !included.motd_lines.is_empty() {
                        motd_lines.extend(included.motd_lines);
                    }

                    if !included.global_refuse_options.is_empty() {
                        global_refuse_directives.extend(included.global_refuse_options);
                    }

                    if let Some((components, origin)) = included.global_bandwidth_limit {
                        if let Some((existing, existing_origin)) = &global_bwlimit {
                            if existing != &components {
                                let existing_line = existing_origin.line;
                                return Err(config_parse_error(
                                    &origin.path,
                                    origin.line,
                                    format!(
                                        "duplicate 'bwlimit' directive in global section (previously defined on line {existing_line})"
                                    ),
                                ));
                            }
                        } else {
                            global_bwlimit = Some((components, origin));
                        }
                    }

                    if let Some((secrets_path, origin)) = included.global_secrets_file {
                        if let Some((existing, existing_origin)) = &global_secrets_file {
                            if existing != &secrets_path {
                                let existing_line = existing_origin.line;
                                return Err(config_parse_error(
                                    &origin.path,
                                    origin.line,
                                    format!(
                                        "duplicate 'secrets file' directive in global section (previously defined on line {existing_line})"
                                    ),
                                ));
                            }
                        } else {
                            global_secrets_file = Some((secrets_path, origin));
                        }
                    }

                    if let Some((incoming, origin)) = included.global_incoming_chmod {
                        if let Some((existing, existing_origin)) = &global_incoming_chmod {
                            if existing != &incoming {
                                let existing_line = existing_origin.line;
                                return Err(config_parse_error(
                                    &origin.path,
                                    origin.line,
                                    format!(
                                        "duplicate 'incoming chmod' directive in global section (previously defined on line {existing_line})"
                                    ),
                                ));
                            }
                        } else {
                            global_incoming_chmod = Some((incoming, origin));
                        }
                    }

                    if let Some((outgoing, origin)) = included.global_outgoing_chmod {
                        if let Some((existing, existing_origin)) = &global_outgoing_chmod {
                            if existing != &outgoing {
                                let existing_line = existing_origin.line;
                                return Err(config_parse_error(
                                    &origin.path,
                                    origin.line,
                                    format!(
                                        "duplicate 'outgoing chmod' directive in global section (previously defined on line {existing_line})"
                                    ),
                                ));
                            }
                        } else {
                            global_outgoing_chmod = Some((outgoing, origin));
                        }
                    }
                }
                "motd file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'motd file' directive must not be empty",
                        ));
                    }

                    let motd_path = resolve_config_relative_path(path, trimmed);
                    let contents = fs::read_to_string(&motd_path).map_err(|error| {
                        let motd_display = motd_path.display();
                        config_parse_error(
                            path,
                            line_number,
                            format!("failed to read motd file '{motd_display}': {error}"),
                        )
                    })?;

                    for raw_line in contents.lines() {
                        motd_lines.push(raw_line.trim_end_matches('\r').to_owned());
                    }
                }
                "motd" => {
                    motd_lines.push(value.trim_end_matches(['\r', '\n']).to_owned());
                }
                "pid file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'pid file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    if let Some((existing, origin)) = &pid_file {
                        if existing != &resolved {
                            let existing_line = origin.line;
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'pid file' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        pid_file = Some((
                            resolved,
                            ConfigDirectiveOrigin {
                                path: canonical.clone(),
                                line: line_number,
                            },
                        ));
                    }
                }
                "reverse lookup" => {
                    let parsed = parse_boolean_directive(value).ok_or_else(|| {
                        config_parse_error(
                            path,
                            line_number,
                            format!("invalid boolean value '{value}' for 'reverse lookup'"),
                        )
                    })?;

                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &reverse_lookup {
                        if *existing != parsed {
                            let existing_line = existing_origin.line;
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'reverse lookup' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        reverse_lookup = Some((parsed, origin));
                    }
                }
                "bwlimit" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'bwlimit' directive must not be empty",
                        ));
                    }

                    let components = parse_config_bwlimit(value, path, line_number)?;
                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &global_bwlimit {
                        if existing != &components {
                            let existing_line = existing_origin.line;
                            return Err(config_parse_error(
                                &origin.path,
                                origin.line,
                                format!(
                                    "duplicate 'bwlimit' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        global_bwlimit = Some((components, origin));
                    }
                }
                "secrets file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'secrets file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    let validated = validate_secrets_file(&resolved, path, line_number)?;
                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &global_secrets_file {
                        if existing != &validated {
                            let existing_line = existing_origin.line;
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'secrets file' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        global_secrets_file = Some((validated, origin));
                    }
                }
                "incoming chmod" | "incoming-chmod" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'incoming chmod' directive must not be empty",
                        ));
                    }

                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &global_incoming_chmod {
                        if existing != value {
                            let existing_line = existing_origin.line;
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'incoming chmod' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        global_incoming_chmod = Some((value.to_owned(), origin));
                    }
                }
                "outgoing chmod" | "outgoing-chmod" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'outgoing chmod' directive must not be empty",
                        ));
                    }

                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &global_outgoing_chmod {
                        if existing != value {
                            let existing_line = existing_origin.line;
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'outgoing chmod' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        global_outgoing_chmod = Some((value.to_owned(), origin));
                    }
                }
                "lock file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'lock file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &lock_file {
                        if existing != &resolved {
                            let existing_line = existing_origin.line;
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'lock file' directive in global section (previously defined on line {existing_line})"
                                ),
                            ));
                        }
                    } else {
                        lock_file = Some((resolved, origin));
                    }
                }
                _ => {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "directive outside module section",
                    ));
                }
            }
        }

        if let Some(builder) = current {
            let default_secrets = global_secrets_file.as_ref().map(|(p, _)| p.as_path());
            let default_incoming =
                global_incoming_chmod.as_ref().map(|(value, _)| value.as_str());
            let default_outgoing =
                global_outgoing_chmod.as_ref().map(|(value, _)| value.as_str());
            modules.push(builder.finish(
                path,
                default_secrets,
                default_incoming,
                default_outgoing,
            )?);
        }

        Ok(ParsedConfigModules {
            modules,
            global_refuse_options: global_refuse_directives,
            motd_lines,
            pid_file,
            reverse_lookup,
            lock_file,
            global_bandwidth_limit: global_bwlimit,
            global_secrets_file,
            global_incoming_chmod,
            global_outgoing_chmod,
        })
    })();

    stack.pop();
    result
}

fn resolve_config_relative_path(config_path: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }

    if let Some(parent) = config_path.parent() {
        parent.join(candidate)
    } else {
        candidate.to_path_buf()
    }
}

#[cfg(test)]
mod config_parsing_tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    fn write_config(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(content.as_bytes()).expect("write config");
        file.flush().expect("flush");
        file
    }

    // --- resolve_config_relative_path tests ---

    #[test]
    fn resolve_config_relative_path_absolute() {
        let result = resolve_config_relative_path(Path::new("/etc/rsyncd.conf"), "/var/run/rsync.pid");
        assert_eq!(result, PathBuf::from("/var/run/rsync.pid"));
    }

    #[test]
    fn resolve_config_relative_path_relative() {
        let result = resolve_config_relative_path(Path::new("/etc/rsyncd.conf"), "rsync.pid");
        assert_eq!(result, PathBuf::from("/etc/rsync.pid"));
    }

    #[test]
    fn resolve_config_relative_path_nested() {
        let result = resolve_config_relative_path(Path::new("/etc/rsync/main.conf"), "sub/file.txt");
        assert_eq!(result, PathBuf::from("/etc/rsync/sub/file.txt"));
    }

    #[test]
    fn resolve_config_relative_path_no_parent() {
        let result = resolve_config_relative_path(Path::new("config.conf"), "relative.txt");
        assert_eq!(result, PathBuf::from("relative.txt"));
    }

    // --- parse_config_modules basic tests ---

    #[test]
    fn parse_empty_config() {
        let file = write_config("");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules.is_empty());
        assert!(result.motd_lines.is_empty());
        assert!(result.pid_file.is_none());
    }

    #[test]
    fn parse_comments_and_blanks() {
        let file = write_config("# Comment line\n\n; Another comment\n   \n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules.is_empty());
    }

    #[test]
    fn parse_single_module() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create module dir");

        let config = format!(
            "[mymodule]\npath = {}\ncomment = Test module\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "mymodule");
        assert_eq!(result.modules[0].path, module_path);
        assert_eq!(result.modules[0].comment, Some("Test module".to_owned()));
    }

    #[test]
    fn parse_multiple_modules() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("data1");
        let path2 = dir.path().join("data2");
        fs::create_dir(&path1).expect("create dir 1");
        fs::create_dir(&path2).expect("create dir 2");

        let config = format!(
            "[mod1]\npath = {}\n\n[mod2]\npath = {}\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert_eq!(result.modules[0].name, "mod1");
        assert_eq!(result.modules[1].name, "mod2");
    }

    // --- Module header error tests ---

    #[test]
    fn parse_unterminated_module_header() {
        let file = write_config("[unclosed\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("unterminated"));
    }

    #[test]
    fn parse_empty_module_name() {
        let file = write_config("[]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn parse_module_name_with_slash() {
        let file = write_config("[bad/name]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("path separator"));
    }

    #[test]
    fn parse_trailing_chars_after_header() {
        let file = write_config("[module] extra\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("unexpected characters"));
    }

    #[test]
    fn parse_trailing_comment_after_header() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[module] # comment\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
    }

    // --- Directive parsing tests ---

    #[test]
    fn parse_missing_equals() {
        let file = write_config("[module]\npath /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("key = value"));
    }

    #[test]
    fn parse_directive_outside_module() {
        let file = write_config("unknown = value\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("outside module"));
    }

    // --- Global directive tests ---

    #[test]
    fn parse_global_pid_file() {
        let file = write_config("pid file = /var/run/rsync.pid\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.pid_file.is_some());
        let (path, _) = result.pid_file.unwrap();
        assert!(path.ends_with("rsync.pid"));
    }

    #[test]
    fn parse_global_reverse_lookup_true() {
        let file = write_config("reverse lookup = yes\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.reverse_lookup, Some((true, ConfigDirectiveOrigin { path: file.path().canonicalize().unwrap(), line: 1 })));
    }

    #[test]
    fn parse_global_reverse_lookup_false() {
        let file = write_config("reverse lookup = no\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (value, _) = result.reverse_lookup.unwrap();
        assert!(!value);
    }

    #[test]
    fn parse_global_lock_file() {
        let file = write_config("lock file = /var/lock/rsync.lock\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.lock_file.is_some());
    }

    #[test]
    fn parse_global_bwlimit() {
        let file = write_config("bwlimit = 1000\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.global_bandwidth_limit.is_some());
    }

    #[test]
    fn parse_global_incoming_chmod() {
        let file = write_config("incoming chmod = u+rwx,g+rx\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (value, _) = result.global_incoming_chmod.unwrap();
        assert_eq!(value, "u+rwx,g+rx");
    }

    #[test]
    fn parse_global_outgoing_chmod() {
        let file = write_config("outgoing chmod = a+r\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (value, _) = result.global_outgoing_chmod.unwrap();
        assert_eq!(value, "a+r");
    }

    // --- MOTD tests ---

    #[test]
    fn parse_inline_motd() {
        let file = write_config("motd = Welcome to rsync\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.motd_lines, vec!["Welcome to rsync"]);
    }

    #[test]
    fn parse_motd_file() {
        let motd_file = write_config("Line 1\nLine 2\nLine 3\n");
        let config = format!("motd file = {}\n", motd_file.path().display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.motd_lines.len(), 3);
        assert_eq!(result.motd_lines[0], "Line 1");
    }

    // --- Module directive tests ---

    #[test]
    fn parse_module_read_only() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nread only = no\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].read_only);
    }

    #[test]
    fn parse_module_write_only() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nwrite only = yes\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].write_only);
    }

    #[test]
    fn parse_module_use_chroot() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nuse chroot = false\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].use_chroot);
    }

    #[test]
    fn parse_module_numeric_ids() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nnumeric ids = true\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].numeric_ids);
    }

    #[test]
    fn parse_module_list() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nlist = no\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].listable);
    }

    #[test]
    fn parse_module_uid_gid() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nuid = 1000\ngid = 1000\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].uid, Some(1000));
        assert_eq!(result.modules[0].gid, Some(1000));
    }

    #[test]
    fn parse_module_timeout() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\ntimeout = 300\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].timeout.unwrap().get(), 300);
    }

    #[test]
    fn parse_module_max_connections() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nmax connections = 10\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].max_connections.unwrap().get(), 10);
    }

    // --- Include directive tests ---

    #[test]
    fn parse_include_directive() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let included = format!("[included_mod]\npath = {}\n", path.display());
        let include_file = write_config(&included);

        let main_config = format!("include = {}\n", include_file.path().display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "included_mod");
    }

    #[test]
    fn parse_recursive_include_detected() {
        let dir = TempDir::new().expect("create temp dir");
        let config_path = dir.path().join("config.conf");

        // Write config that includes itself
        let content = format!("include = {}\n", config_path.display());
        fs::write(&config_path, &content).expect("write config");

        let err = parse_config_modules(&config_path).expect_err("should fail");
        assert!(err.to_string().contains("recursive include"));
    }

    // --- Empty value error tests ---

    #[test]
    fn parse_empty_path_errors() {
        let file = write_config("[mod]\npath = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_empty_pid_file_errors() {
        let file = write_config("pid file = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_empty_include_errors() {
        let file = write_config("include = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_empty_bwlimit_errors() {
        let file = write_config("bwlimit = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    // --- Duplicate directive tests ---

    #[test]
    fn parse_duplicate_pid_file_errors() {
        let file = write_config("pid file = /var/run/a.pid\npid file = /var/run/b.pid\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn parse_duplicate_reverse_lookup_errors() {
        let file = write_config("reverse lookup = yes\nreverse lookup = no\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    // --- Invalid boolean tests ---

    #[test]
    fn parse_invalid_boolean_errors() {
        let file = write_config("[mod]\npath = /tmp\nread only = maybe\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid boolean"));
    }

    // --- Case insensitivity tests ---

    #[test]
    fn parse_keys_case_insensitive() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\nPATH = {}\nREAD ONLY = NO\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
        assert!(!result.modules[0].read_only);
    }

    // --- Config file not found ---

    #[test]
    fn parse_nonexistent_config() {
        let err = parse_config_modules(Path::new("/nonexistent/config.conf"))
            .expect_err("should fail");
        assert!(err.to_string().contains("failed to"));
    }
}
