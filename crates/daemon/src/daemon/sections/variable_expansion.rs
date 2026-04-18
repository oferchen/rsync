/// Context for expanding `%`-delimited variables in daemon config strings.
///
/// Upstream: `loadparm.c:lp_string()` performs `%`-variable substitution at
/// parameter retrieval time, using connection-specific values such as the
/// client address, hostname, module name, and module path.
struct VarExpansionContext<'a> {
    /// Module name from the daemon config.
    module_name: &'a str,
    /// Filesystem path of the module root.
    module_path: &'a str,
    /// Peer IP address string.
    client_addr: &'a str,
    /// Resolved peer hostname, or falls back to `client_addr` when unavailable.
    client_host: &'a str,
}

/// Expands `%`-delimited variables in a daemon config string value.
///
/// Upstream rsync's `loadparm.c:lp_string()` substitutes `%VARIABLE%` tokens
/// when retrieving string parameters at connection time. The supported variables
/// are:
///
/// - `%DIFFHOST%` - the client's hostname (reverse DNS), falls back to address
/// - `%MODULE%` - the module name
/// - `%RSYNC_MODULE_NAME%` - same as `%MODULE%`
/// - `%RSYNC_MODULE_PATH%` - the module's configured path
/// - `%ADDR%` - the client's IP address
/// - `%%` - literal `%`
/// - Unknown `%FOO%` tokens are left as-is
///
/// upstream: `loadparm.c` - `lp_string()` calls `alloc_sub_advanced()` which
/// walks the format string replacing `%`-delimited variable names.
fn expand_config_vars(template: &str, ctx: &VarExpansionContext<'_>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(pct_pos) = rest.find('%') {
        result.push_str(&rest[..pct_pos]);
        rest = &rest[pct_pos + 1..];

        if rest.starts_with('%') {
            result.push('%');
            rest = &rest[1..];
            continue;
        }

        match find_closing_percent(rest) {
            Some(end) => {
                let var_name = &rest[..end];
                match resolve_variable(var_name, ctx) {
                    Some(value) => result.push_str(value),
                    None => {
                        result.push('%');
                        result.push_str(var_name);
                        result.push('%');
                    }
                }
                rest = &rest[end + 1..];
            }
            None => {
                result.push('%');
            }
        }
    }

    result.push_str(rest);
    result
}

/// Returns the byte offset of the next `%` in `s`, or `None` if absent.
fn find_closing_percent(s: &str) -> Option<usize> {
    s.find('%')
}

/// Maps a variable name to its substitution value.
///
/// Returns `None` for unrecognized variable names, which causes the
/// caller to preserve the original `%NAME%` token verbatim.
fn resolve_variable<'a>(name: &str, ctx: &VarExpansionContext<'a>) -> Option<&'a str> {
    match name {
        "DIFFHOST" => Some(ctx.client_host),
        "MODULE" | "RSYNC_MODULE_NAME" => Some(ctx.module_name),
        "RSYNC_MODULE_PATH" => Some(ctx.module_path),
        "ADDR" => Some(ctx.client_addr),
        _ => None,
    }
}

/// Applies `%`-variable expansion to all path-type fields of a module definition.
///
/// Called after module selection when a client connects, before the module path
/// is validated or used for chroot. Expands variables in: `path`, `temp_dir`,
/// `log_file`, `secrets_file`, `exclude_from`, `include_from`.
///
/// upstream: `loadparm.c` - string parameters are expanded via `lp_string()`
/// which calls `alloc_sub_advanced()` for each access.
fn expand_module_vars(
    module: &mut ModuleDefinition,
    client_addr: &str,
    client_host: &str,
) {
    let ctx = VarExpansionContext {
        module_name: &module.name.clone(),
        module_path: &module.path.display().to_string(),
        client_addr,
        client_host,
    };

    module.path = PathBuf::from(expand_config_vars(&module.path.display().to_string(), &ctx));

    if let Some(ref dir) = module.temp_dir {
        module.temp_dir = Some(expand_config_vars(dir, &ctx));
    }

    if let Some(ref path) = module.log_file {
        module.log_file = Some(PathBuf::from(expand_config_vars(
            &path.display().to_string(),
            &ctx,
        )));
    }

    if let Some(ref path) = module.secrets_file {
        module.secrets_file = Some(PathBuf::from(expand_config_vars(
            &path.display().to_string(),
            &ctx,
        )));
    }

    if let Some(ref path) = module.exclude_from {
        module.exclude_from = Some(PathBuf::from(expand_config_vars(
            &path.display().to_string(),
            &ctx,
        )));
    }

    if let Some(ref path) = module.include_from {
        module.include_from = Some(PathBuf::from(expand_config_vars(
            &path.display().to_string(),
            &ctx,
        )));
    }
}

/// Context for expanding single-character `%` variables in daemon paths.
///
/// Upstream rsync expands `%`-escapes in certain config string values at
/// runtime - for example `log file`, `early_exec`, `pre-xfer exec`, and
/// `post-xfer exec`. The supported escapes mirror a subset of the log format
/// variables but apply to path/command strings rather than per-file log lines.
///
/// upstream: `log.c:lp_do_log_file()` and `clientserver.c` expand `%P`, `%m`,
/// `%u`, and `%%` in path contexts.
struct PathExpansionContext<'a> {
    /// Filesystem path of the module root (`%P`).
    module_path: &'a str,
    /// Module name from the daemon config (`%m`).
    module_name: &'a str,
    /// Authenticated username, or empty if anonymous (`%u`).
    username: &'a str,
    /// Peer IP address string (`%a`).
    remote_addr: &'a str,
    /// Resolved peer hostname (`%h`).
    hostname: &'a str,
    /// Daemon process ID (`%p`).
    pid: u32,
}

/// Expands single-character `%` escapes in a daemon path or exec command string.
///
/// Processes `%X` escape sequences by substituting the corresponding field from
/// `ctx`. Supports the path-relevant subset of log format escapes:
///
/// - `%P` - module path
/// - `%m` - module name
/// - `%u` - authenticated username
/// - `%a` - remote IP address
/// - `%h` - remote hostname
/// - `%p` - daemon process ID
/// - `%%` - literal `%`
///
/// Unknown escapes are passed through verbatim, matching upstream behaviour.
///
/// upstream: `log.c` and `clientserver.c` - path strings are expanded at
/// connection time using the active module and session context.
fn expand_daemon_path(template: &str, ctx: &PathExpansionContext<'_>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            result.push(ch);
            continue;
        }

        match chars.next() {
            Some('P') => result.push_str(ctx.module_path),
            Some('m') => result.push_str(ctx.module_name),
            Some('u') => result.push_str(ctx.username),
            Some('a') => result.push_str(ctx.remote_addr),
            Some('h') => result.push_str(ctx.hostname),
            Some('p') => push_u32(&mut result, ctx.pid),
            Some('%') => result.push('%'),
            Some(other) => {
                result.push('%');
                result.push(other);
            }
            None => {
                result.push('%');
            }
        }
    }

    result
}

/// Applies single-character `%`-escape expansion to exec command strings.
///
/// Expands `%P`, `%m`, `%u`, `%a`, `%h`, `%p`, and `%%` in the exec command
/// template using the provided path expansion context. Called before passing
/// exec commands to the shell.
///
/// upstream: `clientserver.c` - exec command strings are expanded at runtime
/// before being passed to `sh -c`.
fn expand_exec_command(command: &str, ctx: &PathExpansionContext<'_>) -> String {
    expand_daemon_path(command, ctx)
}

/// Applies single-character `%`-escape expansion to a log file path.
///
/// Expands `%P`, `%m`, `%u`, `%a`, `%h`, `%p`, and `%%` in the log file path
/// using the provided path expansion context. Called when opening a per-module
/// log file at connection time.
///
/// upstream: `log.c:lp_do_log_file()` - the log file path is expanded at
/// connection time using the current module and session context.
#[allow(dead_code)] // Wired when per-module log files are opened at connection time
fn expand_log_file_path(path: &str, ctx: &PathExpansionContext<'_>) -> PathBuf {
    PathBuf::from(expand_daemon_path(path, ctx))
}

#[cfg(test)]
mod variable_expansion_tests {
    use super::*;

    fn sample_ctx<'a>() -> VarExpansionContext<'a> {
        VarExpansionContext {
            module_name: "backup",
            module_path: "/srv/backup",
            client_addr: "192.168.1.100",
            client_host: "client.example.com",
        }
    }


    #[test]
    fn expand_diffhost() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/data/%DIFFHOST%/files", &ctx),
            "/data/client.example.com/files"
        );
    }

    #[test]
    fn expand_module() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/srv/%MODULE%", &ctx),
            "/srv/backup"
        );
    }

    #[test]
    fn expand_rsync_module_name() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/srv/%RSYNC_MODULE_NAME%/data", &ctx),
            "/srv/backup/data"
        );
    }

    #[test]
    fn expand_rsync_module_path() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("%RSYNC_MODULE_PATH%/sub", &ctx),
            "/srv/backup/sub"
        );
    }

    #[test]
    fn expand_addr() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/logs/%ADDR%.log", &ctx),
            "/logs/192.168.1.100.log"
        );
    }

    #[test]
    fn expand_literal_percent() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("100%%", &ctx), "100%");
    }

    #[test]
    fn expand_double_percent_mid_string() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("a%%b%%c", &ctx),
            "a%b%c"
        );
    }


    #[test]
    fn expand_unknown_variable_preserved() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/data/%UNKNOWN%/files", &ctx),
            "/data/%UNKNOWN%/files"
        );
    }

    #[test]
    fn expand_empty_string() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("", &ctx), "");
    }

    #[test]
    fn expand_no_variables() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("/plain/path", &ctx), "/plain/path");
    }

    #[test]
    fn expand_trailing_percent_no_close() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/path/%MODULE", &ctx),
            "/path/%MODULE"
        );
    }

    #[test]
    fn expand_empty_variable_name() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("/path/%%/dir", &ctx), "/path/%/dir");
    }

    #[test]
    fn expand_multiple_variables() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/data/%MODULE%/%ADDR%/files", &ctx),
            "/data/backup/192.168.1.100/files"
        );
    }

    #[test]
    fn expand_adjacent_variables() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("%MODULE%%ADDR%", &ctx),
            "backup192.168.1.100"
        );
    }

    #[test]
    fn expand_all_variables_combined() {
        let ctx = sample_ctx();
        let input = "%DIFFHOST%-%MODULE%-%RSYNC_MODULE_NAME%-%RSYNC_MODULE_PATH%-%ADDR%";
        let expected =
            "client.example.com-backup-backup-/srv/backup-192.168.1.100";
        assert_eq!(expand_config_vars(input, &ctx), expected);
    }

    #[test]
    fn expand_variable_at_start() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("%MODULE%/data", &ctx), "backup/data");
    }

    #[test]
    fn expand_variable_at_end() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("/data/%MODULE%", &ctx), "/data/backup");
    }

    #[test]
    fn expand_only_variable() {
        let ctx = sample_ctx();
        assert_eq!(expand_config_vars("%MODULE%", &ctx), "backup");
    }

    #[test]
    fn expand_percent_before_variable() {
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("100%% %MODULE%", &ctx),
            "100% backup"
        );
    }


    #[test]
    fn resolve_diffhost() {
        let ctx = sample_ctx();
        assert_eq!(resolve_variable("DIFFHOST", &ctx), Some("client.example.com"));
    }

    #[test]
    fn resolve_module() {
        let ctx = sample_ctx();
        assert_eq!(resolve_variable("MODULE", &ctx), Some("backup"));
    }

    #[test]
    fn resolve_rsync_module_name() {
        let ctx = sample_ctx();
        assert_eq!(resolve_variable("RSYNC_MODULE_NAME", &ctx), Some("backup"));
    }

    #[test]
    fn resolve_rsync_module_path() {
        let ctx = sample_ctx();
        assert_eq!(resolve_variable("RSYNC_MODULE_PATH", &ctx), Some("/srv/backup"));
    }

    #[test]
    fn resolve_addr() {
        let ctx = sample_ctx();
        assert_eq!(resolve_variable("ADDR", &ctx), Some("192.168.1.100"));
    }

    #[test]
    fn resolve_unknown() {
        let ctx = sample_ctx();
        assert_eq!(resolve_variable("NOPE", &ctx), None);
    }


    #[test]
    fn expand_module_vars_expands_path() {
        let mut module = ModuleDefinition {
            name: "photos".to_owned(),
            path: PathBuf::from("/data/%MODULE%"),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(module.path, PathBuf::from("/data/photos"));
    }

    #[test]
    fn expand_module_vars_expands_temp_dir() {
        let mut module = ModuleDefinition {
            name: "docs".to_owned(),
            path: PathBuf::from("/srv/docs"),
            temp_dir: Some("/tmp/%MODULE%".to_owned()),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(module.temp_dir.as_deref(), Some("/tmp/docs"));
    }

    #[test]
    fn expand_module_vars_expands_log_file() {
        let mut module = ModuleDefinition {
            name: "logs".to_owned(),
            path: PathBuf::from("/srv/logs"),
            log_file: Some(PathBuf::from("/var/log/%MODULE%.log")),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(module.log_file, Some(PathBuf::from("/var/log/logs.log")));
    }

    #[test]
    fn expand_module_vars_expands_secrets_file() {
        let mut module = ModuleDefinition {
            name: "secure".to_owned(),
            path: PathBuf::from("/srv/secure"),
            secrets_file: Some(PathBuf::from("/etc/%MODULE%.secrets")),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(
            module.secrets_file,
            Some(PathBuf::from("/etc/secure.secrets"))
        );
    }

    #[test]
    fn expand_module_vars_expands_exclude_from() {
        let mut module = ModuleDefinition {
            name: "data".to_owned(),
            path: PathBuf::from("/srv/data"),
            exclude_from: Some(PathBuf::from("/etc/%MODULE%.exclude")),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(
            module.exclude_from,
            Some(PathBuf::from("/etc/data.exclude"))
        );
    }

    #[test]
    fn expand_module_vars_expands_include_from() {
        let mut module = ModuleDefinition {
            name: "data".to_owned(),
            path: PathBuf::from("/srv/data"),
            include_from: Some(PathBuf::from("/etc/%MODULE%.include")),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(
            module.include_from,
            Some(PathBuf::from("/etc/data.include"))
        );
    }

    #[test]
    fn expand_module_vars_leaves_none_fields_unchanged() {
        let mut module = ModuleDefinition {
            name: "plain".to_owned(),
            path: PathBuf::from("/srv/plain"),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(module.path, PathBuf::from("/srv/plain"));
        assert!(module.temp_dir.is_none());
        assert!(module.log_file.is_none());
        assert!(module.secrets_file.is_none());
        assert!(module.exclude_from.is_none());
        assert!(module.include_from.is_none());
    }

    #[test]
    fn expand_module_vars_with_addr_in_path() {
        let mut module = ModuleDefinition {
            name: "perhost".to_owned(),
            path: PathBuf::from("/data/%ADDR%/%MODULE%"),
            ..Default::default()
        };
        expand_module_vars(&mut module, "192.168.1.50", "client.lan");
        assert_eq!(module.path, PathBuf::from("/data/192.168.1.50/perhost"));
    }

    #[test]
    fn expand_module_vars_with_diffhost_in_path() {
        let mut module = ModuleDefinition {
            name: "perhost".to_owned(),
            path: PathBuf::from("/backup/%DIFFHOST%"),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "laptop.home");
        assert_eq!(module.path, PathBuf::from("/backup/laptop.home"));
    }

    #[test]
    fn expand_module_vars_multiple_fields() {
        let mut module = ModuleDefinition {
            name: "multi".to_owned(),
            path: PathBuf::from("/data/%MODULE%"),
            temp_dir: Some("/tmp/%MODULE%".to_owned()),
            log_file: Some(PathBuf::from("/var/log/%MODULE%.log")),
            secrets_file: Some(PathBuf::from("/etc/%MODULE%.secrets")),
            exclude_from: Some(PathBuf::from("/etc/%MODULE%.exclude")),
            include_from: Some(PathBuf::from("/etc/%MODULE%.include")),
            ..Default::default()
        };
        expand_module_vars(&mut module, "10.0.0.1", "host.local");
        assert_eq!(module.path, PathBuf::from("/data/multi"));
        assert_eq!(module.temp_dir.as_deref(), Some("/tmp/multi"));
        assert_eq!(module.log_file, Some(PathBuf::from("/var/log/multi.log")));
        assert_eq!(
            module.secrets_file,
            Some(PathBuf::from("/etc/multi.secrets"))
        );
        assert_eq!(
            module.exclude_from,
            Some(PathBuf::from("/etc/multi.exclude"))
        );
        assert_eq!(
            module.include_from,
            Some(PathBuf::from("/etc/multi.include"))
        );
    }


    fn sample_path_ctx<'a>() -> PathExpansionContext<'a> {
        PathExpansionContext {
            module_path: "/srv/backup",
            module_name: "backup",
            username: "alice",
            remote_addr: "192.168.1.100",
            hostname: "client.example.com",
            pid: 42,
        }
    }

    #[test]
    fn daemon_path_expand_module_path() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("%P/logs", &ctx), "/srv/backup/logs");
    }

    #[test]
    fn daemon_path_expand_module_name() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_daemon_path("/var/log/%m.log", &ctx),
            "/var/log/backup.log"
        );
    }

    #[test]
    fn daemon_path_expand_username() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_daemon_path("/home/%u/sync", &ctx),
            "/home/alice/sync"
        );
    }

    #[test]
    fn daemon_path_expand_remote_addr() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_daemon_path("/logs/%a.log", &ctx),
            "/logs/192.168.1.100.log"
        );
    }

    #[test]
    fn daemon_path_expand_hostname() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_daemon_path("/logs/%h/data", &ctx),
            "/logs/client.example.com/data"
        );
    }

    #[test]
    fn daemon_path_expand_pid() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_daemon_path("/var/run/rsync.%p.lock", &ctx),
            "/var/run/rsync.42.lock"
        );
    }

    #[test]
    fn daemon_path_expand_literal_percent() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("100%%", &ctx), "100%");
    }

    #[test]
    fn daemon_path_expand_unknown_escape_passthrough() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("/path/%Z/data", &ctx), "/path/%Z/data");
    }

    #[test]
    fn daemon_path_expand_trailing_percent() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("/path%", &ctx), "/path%");
    }

    #[test]
    fn daemon_path_expand_empty_string() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("", &ctx), "");
    }

    #[test]
    fn daemon_path_expand_no_escapes() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("/plain/path", &ctx), "/plain/path");
    }

    #[test]
    fn daemon_path_expand_multiple_escapes() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_daemon_path("/var/log/%m/%h.log", &ctx),
            "/var/log/backup/client.example.com.log"
        );
    }

    #[test]
    fn daemon_path_expand_adjacent_escapes() {
        let ctx = sample_path_ctx();
        assert_eq!(expand_daemon_path("%m%P", &ctx), "backup/srv/backup");
    }

    #[test]
    fn daemon_path_expand_empty_username() {
        let ctx = PathExpansionContext {
            username: "",
            ..sample_path_ctx()
        };
        assert_eq!(expand_daemon_path("/home/%u/data", &ctx), "/home//data");
    }

    #[test]
    fn daemon_path_expand_all_escapes() {
        let ctx = sample_path_ctx();
        let result = expand_daemon_path("%P-%m-%u-%a-%h-%p", &ctx);
        assert_eq!(
            result,
            "/srv/backup-backup-alice-192.168.1.100-client.example.com-42"
        );
    }


    #[test]
    fn exec_command_expands_module_name() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_exec_command("echo %m", &ctx),
            "echo backup"
        );
    }

    #[test]
    fn exec_command_expands_multiple_vars() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_exec_command("/usr/local/bin/notify --module=%m --user=%u --host=%h", &ctx),
            "/usr/local/bin/notify --module=backup --user=alice --host=client.example.com"
        );
    }


    #[test]
    fn log_file_path_expands_module_name() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_log_file_path("/var/log/rsync/%m.log", &ctx),
            PathBuf::from("/var/log/rsync/backup.log")
        );
    }

    #[test]
    fn log_file_path_expands_module_path() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_log_file_path("%P/rsync.log", &ctx),
            PathBuf::from("/srv/backup/rsync.log")
        );
    }

    #[test]
    fn log_file_path_no_escapes() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expand_log_file_path("/var/log/rsync.log", &ctx),
            PathBuf::from("/var/log/rsync.log")
        );
    }
}
