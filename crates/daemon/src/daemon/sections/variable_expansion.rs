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

    // --- expand_config_vars: individual variable tests ---

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

    // --- Unknown and edge cases ---

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

    // --- resolve_variable tests ---

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

    // --- expand_module_vars tests ---

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
}
