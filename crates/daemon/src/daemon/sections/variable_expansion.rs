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
/// - Any other all-uppercase `%NAME%` token is looked up in the process
///   environment (upstream `loadparm.c:expand_vars` calls `getenv`); a set
///   variable is substituted, an unset one is left as-is
/// - Any remaining `%FOO%` token is left as-is
///
/// upstream: `loadparm.c:expand_vars()` walks the string and, for every
/// `%UPPERCASE...%` token, calls `getenv()` on the name between the percents,
/// substituting the value when set and leaving the raw token unchanged when not.
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
                    None => match env_expansion(var_name) {
                        Some(value) => result.push_str(&value),
                        None => {
                            result.push('%');
                            result.push_str(var_name);
                            result.push('%');
                        }
                    },
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
/// Returns `None` for names that are not built-in connection variables; the
/// caller then falls back to an environment lookup (`env_expansion`) and, only
/// if that also misses, preserves the original `%NAME%` token verbatim.
fn resolve_variable<'a>(name: &str, ctx: &VarExpansionContext<'a>) -> Option<&'a str> {
    match name {
        "DIFFHOST" => Some(ctx.client_host),
        "MODULE" | "RSYNC_MODULE_NAME" => Some(ctx.module_name),
        "RSYNC_MODULE_PATH" => Some(ctx.module_path),
        "ADDR" => Some(ctx.client_addr),
        _ => None,
    }
}

/// Looks up an all-uppercase `%NAME%` token in the process environment.
///
/// Returns the environment value when `name` is a non-empty run of
/// `[A-Z0-9_]` and the variable is set, otherwise `None` so the caller leaves
/// the literal `%NAME%` token unchanged.
///
/// upstream: `loadparm.c:expand_vars` (~185) calls `getenv()` on every
/// `%UPPERCASE...%` token that is not a built-in name; an unset variable leaves
/// the raw token in place.
fn env_expansion(name: &str) -> Option<String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    std::env::var(name).ok()
}

/// Applies `%`-variable expansion to all path-type fields of a module definition.
///
/// Called after module selection when a client connects, before the module path
/// is validated or used for chroot. Expands variables in: `path`, `temp_dir`,
/// `log_file`, `secrets_file`, `exclude_from`, `include_from`.
///
/// upstream: `loadparm.c` - string parameters are expanded via `lp_string()`
/// which calls `alloc_sub_advanced()` for each access.
fn expand_module_vars(module: &mut ModuleDefinition, client_addr: &str, client_host: &str) {
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

/// The shell quoting context a substituted value lands in.
///
/// upstream: `loadparm.c:167-171` `enum shell_quote_context`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum ShellQuoteContext {
    #[default]
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
}

/// Tracks the shell quoting context across the literal bytes of a hook template.
///
/// Only template bytes are fed to it; a substituted value is never re-scanned,
/// so a value cannot open or close a quoted run.
///
/// upstream: `loadparm.c:289-310` - the tail of `expand_vars()` under
/// `shell_escape`.
#[derive(Default)]
struct ShellQuoteScanner {
    context: ShellQuoteContext,
    escaped: bool,
}

impl ShellQuoteScanner {
    /// The context a value substituted at the current position lands in.
    fn context(&self) -> ShellQuoteContext {
        self.context
    }

    /// Advances the tracker over one literal template character.
    fn advance(&mut self, ch: char) {
        if self.context == ShellQuoteContext::SingleQuoted {
            // Nothing is special inside '...', not even a backslash; only the
            // closing quote ends it. upstream: loadparm.c:290-294
            if ch == '\'' {
                self.context = ShellQuoteContext::Unquoted;
            }
        } else if self.escaped {
            self.escaped = false;
        } else if ch == '\\' {
            self.escaped = true;
        } else if self.context == ShellQuoteContext::DoubleQuoted {
            // A single quote inside "..." is literal and must not be taken as
            // opening a single-quoted run - doing so would de-sync the tracker
            // and escape a later value for the wrong context.
            // upstream: loadparm.c:299-305
            if ch == '"' {
                self.context = ShellQuoteContext::Unquoted;
            }
        } else if ch == '\'' {
            self.context = ShellQuoteContext::SingleQuoted;
        } else if ch == '"' {
            self.context = ShellQuoteContext::DoubleQuoted;
        }
    }
}

/// Reports whether a value carries a character a shell could act on.
///
/// Quoting alone cannot be relied on: context-aware escaping is correct for
/// exactly one level of shell parsing, and a hook such as
/// `sh -c '... %RSYNC_USER_NAME% ...'` re-parses the word in a second shell
/// that sees the value bare. Peer-supplied values carrying any of these are
/// refused instead.
///
/// upstream: `loadparm.c:179-195` `shell_unsafe_value()`.
fn shell_unsafe_value(value: &str) -> bool {
    // `!` negates in command position, `~` is tilde-expanded, and `{`/`}`
    // brace-expand in bash and zsh; none execute anything on their own, which
    // is why a set built from the obvious metacharacters missed them.
    const UNSAFE: &str = "'\"`$\\;&|<>()*?[]# !~{}";
    value
        .chars()
        .any(|ch| UNSAFE.contains(ch) || (ch as u32) < 0x20 || ch as u32 == 0x7f)
}

/// Escapes a value for the shell quoting context it is substituted into.
///
/// A double-quoted value is deliberately BOTH backslash-escaped and wrapped in
/// single quotes: the wrap is redundant for one level of shell parsing, but a
/// hook that re-parses the word in a second shell has already lost the
/// backslashes and only the quotes still protect it.
///
/// The per-character escape branches are unreachable behind
/// `shell_unsafe_value`, which refuses every character they handle. Upstream
/// keeps both layers too - the refusal is the newer rule laid over the older
/// escaper - and dropping the escaper here would silently diverge if that
/// refusal set were ever narrowed.
///
/// upstream: `loadparm.c:197-235` `expand_vars_shell_escape()`.
fn shell_escape_value(value: &str, context: ShellQuoteContext) -> String {
    let wrap = context != ShellQuoteContext::SingleQuoted;
    let mut escaped = String::with_capacity(value.len() + 2);

    if wrap {
        escaped.push('\'');
    }
    for ch in value.chars() {
        if context == ShellQuoteContext::DoubleQuoted && matches!(ch, '\\' | '"' | '`' | '$') {
            escaped.push('\\');
            escaped.push(ch);
        } else if ch == '\'' {
            escaped.push_str("'\\''");
        } else {
            escaped.push(ch);
        }
    }
    if wrap {
        escaped.push('\'');
    }

    escaped
}

/// A peer-influenced value carried a character a shell could act on.
///
/// The hook may be an access check, so skipping it is not an option: the
/// daemon fails closed and aborts the transfer.
///
/// upstream: `loadparm.c:267-274` - `rprintf(FLOG, ...)` followed by
/// `exit_cleanup(RERR_UNSUPPORTED)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellHookRefusal {
    /// The token as written in the template, e.g. `%RSYNC_USER_NAME%` or `%u`.
    token: String,
}

impl ShellHookRefusal {
    /// The daemon-log line upstream emits before aborting.
    ///
    /// upstream: `loadparm.c:270-272`.
    pub(crate) fn log_line(&self) -> String {
        format!(
            "refusing to run shell hook: {} holds a shell metacharacter",
            self.token
        )
    }
}

/// Splits a leading `NAME%` off `rest` when it is a well-formed variable name.
///
/// Requires the upstream shape - an uppercase first letter (`isUpper(f+1)`)
/// followed by `[A-Z0-9_]` and a closing `%`. The well-formedness check is what
/// keeps oc's single-character escapes working: `%P/%m` yields the candidate
/// name `P/`, which is rejected here and falls through to the `%P` reading,
/// while `%PATH%` is a real variable reference and is read as one.
///
/// upstream: `loadparm.c:250-252`.
fn delimited_variable(rest: &str) -> Option<(&str, &str)> {
    let end = rest.find('%')?;
    let name = &rest[..end];
    let mut bytes = name.bytes();

    if !bytes.next()?.is_ascii_uppercase() {
        return None;
    }
    if !bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return None;
    }

    Some((name, &rest[end + 1..]))
}

/// Resolves a delimited `%NAME%` reference for a shell-executed hook.
///
/// The connection variables are read from `ctx` rather than the process
/// environment: upstream sets them on the daemon process before retrieving the
/// hook directive (`clientserver.c:757/770/771/815/920`, all ahead of the
/// retrieval at `:959`), whereas oc sets them on the hook child instead. Any
/// other name falls back to the real environment, matching upstream's `getenv`.
///
/// `RSYNC_REQUEST` and `RSYNC_ARG<n>` are deliberately absent: upstream sets
/// those inside the forked pre-exec child (`clientserver.c:568`), after the
/// directive has already been expanded, so they are not references a template
/// can resolve on either implementation.
fn resolve_hook_variable(name: &str, ctx: &PathExpansionContext<'_>) -> Option<String> {
    match name {
        "RSYNC_MODULE_NAME" => Some(ctx.module_name.to_string()),
        "RSYNC_MODULE_PATH" => Some(ctx.module_path.to_string()),
        "RSYNC_HOST_NAME" => Some(ctx.hostname.to_string()),
        "RSYNC_HOST_ADDR" => Some(ctx.remote_addr.to_string()),
        "RSYNC_USER_NAME" => Some(ctx.username.to_string()),
        _ => env_expansion(name),
    }
}

/// Substitutes one resolved value, refusing or escaping it when peer-influenced.
///
/// Upstream keys the rule on the `RSYNC_` name prefix rather than on where the
/// value came from (`loadparm.c:266`); mirroring that keeps ordinary string
/// params such as `path = /home/%RSYNC_USER_NAME%` verbatim while every value
/// reaching a shell-executed hook is checked.
fn substitute_hook_value(
    token: &str,
    peer_influenced: bool,
    value: &str,
    context: ShellQuoteContext,
) -> Result<String, ShellHookRefusal> {
    if !peer_influenced {
        return Ok(value.to_string());
    }
    if shell_unsafe_value(value) {
        return Err(ShellHookRefusal {
            token: token.to_string(),
        });
    }
    Ok(shell_escape_value(value, context))
}

/// Expands a shell-executed hook command template.
///
/// Handles upstream's `%NAME%` environment references and oc's single-character
/// escapes in ONE walk, so a substituted value is never rescanned by the other
/// form. Delimited names win, because that is the only form upstream has.
///
/// Values that reach a shell-executed hook are escaped for the quoting context
/// they land in, and refused outright when they carry a character a shell could
/// act on.
///
/// upstream: `loadparm.c:237-325` `expand_vars(str, shell_escape=1)`, reached
/// via `FN_LOCAL_STRING_SHELL` for `early exec`, `name converter`,
/// `post-xfer exec` and `pre-xfer exec` (`daemon-parm.h:349/363/365/366`).
fn expand_exec_command(
    command: &str,
    ctx: &PathExpansionContext<'_>,
) -> Result<String, ShellHookRefusal> {
    let mut out = String::with_capacity(command.len());
    let mut scanner = ShellQuoteScanner::default();
    let mut rest = command;

    while let Some(pos) = rest.find('%') {
        push_literal(&mut out, &mut scanner, &rest[..pos]);
        rest = &rest[pos + 1..];

        if let Some(tail) = rest.strip_prefix('%') {
            push_literal(&mut out, &mut scanner, "%%");
            rest = tail;
            continue;
        }

        if let Some((name, tail)) = delimited_variable(rest) {
            match resolve_hook_variable(name, ctx) {
                Some(value) => {
                    let token = format!("%{name}%");
                    let peer_influenced = name.starts_with("RSYNC_");
                    out.push_str(&substitute_hook_value(
                        &token,
                        peer_influenced,
                        &value,
                        scanner.context(),
                    )?);
                }
                // upstream leaves an unresolved reference verbatim; it must not
                // then be re-read as a single-character escape.
                None => push_literal(&mut out, &mut scanner, &format!("%{name}%")),
            }
            rest = tail;
            continue;
        }

        let mut chars = rest.chars();
        match chars.next() {
            Some(escape) => {
                match single_char_escape(escape, ctx) {
                    Some((value, peer_influenced)) => {
                        let token = format!("%{escape}");
                        out.push_str(&substitute_hook_value(
                            &token,
                            peer_influenced,
                            &value,
                            scanner.context(),
                        )?);
                    }
                    None => push_literal(&mut out, &mut scanner, &format!("%{escape}")),
                }
                rest = chars.as_str();
            }
            None => push_literal(&mut out, &mut scanner, "%"),
        }
    }

    push_literal(&mut out, &mut scanner, rest);
    Ok(out)
}

/// Copies template text through to the output, advancing the quote tracker.
fn push_literal(out: &mut String, scanner: &mut ShellQuoteScanner, text: &str) {
    for ch in text.chars() {
        scanner.advance(ch);
    }
    out.push_str(text);
}

/// Resolves one oc single-character escape, reporting whether the value is
/// peer-influenced.
///
/// ⚠ These escapes are an oc extension: upstream expands only `%NAME%` in
/// config values (`loadparm.c:250`, gated on `isUpper(f+1)` plus a closing
/// `%`), so `%m` stays literal there. `%u`, `%h` and `%a` carry values the peer
/// influences, so they take the same refusal as a `%RSYNC_*%` reference; the
/// rest are operator- or daemon-derived and substitute verbatim.
fn single_char_escape(escape: char, ctx: &PathExpansionContext<'_>) -> Option<(String, bool)> {
    match escape {
        'P' => Some((ctx.module_path.to_string(), false)),
        'm' => Some((ctx.module_name.to_string(), false)),
        'p' => Some((ctx.pid.to_string(), false)),
        'u' => Some((ctx.username.to_string(), true)),
        'a' => Some((ctx.remote_addr.to_string(), true)),
        'h' => Some((ctx.hostname.to_string(), true)),
        _ => None,
    }
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
        assert_eq!(expand_config_vars("/srv/%MODULE%", &ctx), "/srv/backup");
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
        assert_eq!(expand_config_vars("a%%b%%c", &ctx), "a%b%c");
    }

    #[test]
    fn expand_unset_env_variable_preserved() {
        // upstream: loadparm.c:expand_vars - an all-uppercase token that is not
        // a built-in name and is unset in the environment leaves the literal
        // `%TOKEN%` in place (getenv returns NULL, so the raw chars pass
        // through). WHY: a config author's `%FOO%` must not silently vanish when
        // FOO is undefined.
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::test_env::EnvGuard::remove("OC_RSYNC_TEST_EXPAND_UNSET");
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/data/%OC_RSYNC_TEST_EXPAND_UNSET%/files", &ctx),
            "/data/%OC_RSYNC_TEST_EXPAND_UNSET%/files"
        );
    }

    #[test]
    fn expand_env_variable_from_process_environment() {
        // upstream: loadparm.c:expand_vars (~185) calls getenv() on any
        // %UPPERCASE% token that is not a built-in name and substitutes the
        // value when set. WHY: rsyncd.conf paths like `path = %HOME%/rsync` must
        // resolve against the daemon's environment exactly as upstream does.
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::test_env::EnvGuard::set(
            "OC_RSYNC_TEST_EXPAND_VAR",
            std::ffi::OsStr::new("/srv/env"),
        );
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("%OC_RSYNC_TEST_EXPAND_VAR%/rsync", &ctx),
            "/srv/env/rsync"
        );
    }

    #[test]
    fn expand_lowercase_token_not_env_expanded() {
        // A non-uppercase token is not an environment variable name and is left
        // literal even if a same-named var were set, matching upstream's
        // isUpper() gate in expand_vars.
        let ctx = sample_ctx();
        assert_eq!(
            expand_config_vars("/data/%path%/files", &ctx),
            "/data/%path%/files"
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
        assert_eq!(expand_config_vars("/path/%MODULE", &ctx), "/path/%MODULE");
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
        let expected = "client.example.com-backup-backup-/srv/backup-192.168.1.100";
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
        assert_eq!(expand_config_vars("100%% %MODULE%", &ctx), "100% backup");
    }

    #[test]
    fn resolve_diffhost() {
        let ctx = sample_ctx();
        assert_eq!(
            resolve_variable("DIFFHOST", &ctx),
            Some("client.example.com")
        );
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
        assert_eq!(
            resolve_variable("RSYNC_MODULE_PATH", &ctx),
            Some("/srv/backup")
        );
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

    fn expanded(command: &str, ctx: &PathExpansionContext<'_>) -> String {
        expand_exec_command(command, ctx).expect("template must expand")
    }

    #[test]
    fn exec_command_keeps_operator_supplied_values_verbatim() {
        let ctx = sample_path_ctx();
        assert_eq!(expanded("echo %m", &ctx), "echo backup");
    }

    #[test]
    fn exec_command_escapes_peer_influenced_values() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expanded(
                "/usr/local/bin/notify --module=%m --user=%u --host=%h",
                &ctx
            ),
            "/usr/local/bin/notify --module=backup --user='alice' --host='client.example.com'"
        );
    }

    #[test]
    fn exec_command_resolves_delimited_connection_variables() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expanded("notify %RSYNC_MODULE_NAME% %RSYNC_USER_NAME%", &ctx),
            "notify 'backup' 'alice'"
        );
    }

    #[test]
    fn exec_command_leaves_unresolved_reference_verbatim() {
        let ctx = sample_path_ctx();
        assert_eq!(
            expanded("echo %OC_RSYNC_ABSENT_VARIABLE%", &ctx),
            "echo %OC_RSYNC_ABSENT_VARIABLE%"
        );
    }

    #[test]
    fn exec_command_does_not_reread_a_substituted_value() {
        let ctx = PathExpansionContext {
            module_name: "%u",
            ..sample_path_ctx()
        };
        assert_eq!(expanded("echo %m", &ctx), "echo %u");
    }

    #[test]
    fn exec_command_omits_the_wrap_inside_a_single_quoted_run() {
        let ctx = sample_path_ctx();
        assert_eq!(expanded("echo '%u'", &ctx), "echo 'alice'");
    }

    #[test]
    fn exec_command_refuses_before_the_double_quote_escape_can_apply() {
        let ctx = PathExpansionContext {
            username: "a$b",
            ..sample_path_ctx()
        };
        // Every character the double-quoted arm backslash-escapes (`\ " ` $`)
        // is also in the refusal set, so a peer value never reaches that arm.
        // The arm is kept because upstream keeps it: `expand_vars_shell_escape`
        // is written for any value, not just the ones that survive
        // `shell_unsafe_value`. upstream: `loadparm.c:197-235`.
        assert!(expand_exec_command("echo \"%u\"", &ctx).is_err());
    }

    #[test]
    fn exec_command_tracks_quote_context_across_a_closed_run() {
        let ctx = sample_path_ctx();
        assert_eq!(expanded("echo 'x' %u", &ctx), "echo 'x' 'alice'");
    }

    #[test]
    fn exec_command_treats_a_literal_quote_inside_double_quotes_as_text() {
        let ctx = sample_path_ctx();
        // The `'` inside `"..."` must not open a single-quoted run, or the
        // value after it would be escaped for the wrong context.
        // upstream: `loadparm.c:300-305`.
        assert_eq!(expanded("echo \"it's\" %u", &ctx), "echo \"it's\" 'alice'");
    }

    #[test]
    fn exec_command_keeps_a_doubled_percent_literal() {
        let ctx = sample_path_ctx();
        assert_eq!(expanded("fmt %%m %m", &ctx), "fmt %%m backup");
    }

    #[test]
    fn exec_command_refuses_a_peer_value_holding_a_metacharacter() {
        let ctx = PathExpansionContext {
            username: "alice; rm -rf /",
            ..sample_path_ctx()
        };
        let refusal = expand_exec_command("notify --user=%u", &ctx)
            .expect_err("a shell metacharacter must fail closed");
        assert_eq!(
            refusal.log_line(),
            "refusing to run shell hook: %u holds a shell metacharacter"
        );
    }

    #[test]
    fn exec_command_refusal_names_the_delimited_token() {
        let ctx = PathExpansionContext {
            hostname: "a`id`b",
            ..sample_path_ctx()
        };
        let refusal = expand_exec_command("notify %RSYNC_HOST_NAME%", &ctx)
            .expect_err("a shell metacharacter must fail closed");
        assert_eq!(
            refusal.log_line(),
            "refusing to run shell hook: %RSYNC_HOST_NAME% holds a shell metacharacter"
        );
    }

    #[test]
    fn exec_command_refuses_the_full_unsafe_set() {
        for probe in [
            "a'b", "a\"b", "a`b", "a$b", "a\\b", "a;b", "a&b", "a|b", "a<b", "a>b", "a(b", "a)b",
            "a*b", "a?b", "a[b", "a]b", "a#b", "a b", "a!b", "a~b", "a{b", "a}b", "a\nb", "a\x7fb",
        ] {
            let ctx = PathExpansionContext {
                username: probe,
                ..sample_path_ctx()
            };
            assert!(
                expand_exec_command("notify %u", &ctx).is_err(),
                "{probe:?} must be refused"
            );
        }
    }

    #[test]
    fn exec_command_allows_an_operator_value_holding_a_metacharacter() {
        let ctx = PathExpansionContext {
            module_path: "/srv/my backups",
            ..sample_path_ctx()
        };
        // Only `RSYNC_`-named values are checked upstream, so an
        // operator-configured path stays verbatim. upstream: `loadparm.c:266`.
        assert_eq!(expanded("du %P", &ctx), "du /srv/my backups");
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
