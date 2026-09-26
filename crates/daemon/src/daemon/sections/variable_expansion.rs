/// The `RSYNC_*` connection variables a daemon parameter can reference.
///
/// Upstream sets these on the daemon process as the session reaches each one
/// (clientserver.c:757 `RSYNC_MODULE_NAME`, :770-771 `RSYNC_HOST_NAME` and
/// `RSYNC_HOST_ADDR`, :815 `RSYNC_USER_NAME`, :920 `RSYNC_MODULE_PATH`), and
/// every string parameter is expanded through `getenv` when it is first read.
/// oc does not modify its own environment, so it carries the values here; a
/// field left `None` is one upstream has not set yet at that parameter's read
/// point, and the name then falls back to the real environment exactly as
/// upstream's `getenv` would.
#[derive(Clone, Copy, Default)]
struct HookVariables<'a> {
    /// `RSYNC_MODULE_NAME`.
    module_name: Option<&'a str>,
    /// `RSYNC_MODULE_PATH`.
    module_path: Option<&'a str>,
    /// `RSYNC_HOST_NAME`.
    host_name: Option<&'a str>,
    /// `RSYNC_HOST_ADDR`.
    host_addr: Option<&'a str>,
    /// `RSYNC_USER_NAME`; empty for an anonymous module.
    user_name: Option<&'a str>,
}

/// Expands `%NAME%` references in a daemon parameter that is not a shell hook.
///
/// upstream: `loadparm.c:237-325` `expand_vars(str, shell_escape=0)` - values
/// are substituted verbatim, and there are no built-in names, no single-letter
/// escapes and no `%%` escape.
fn expand_config_vars(template: &str, vars: &HookVariables<'_>) -> String {
    match expand_vars(template, vars, false) {
        Ok(expanded) => expanded,
        Err(_) => unreachable!("only a shell-hook expansion refuses a value"),
    }
}

/// Expands the module's path-type parameters the way upstream reads them.
///
/// `secrets file` is read by `auth_server()` (clientserver.c:809), before
/// `RSYNC_USER_NAME` is set, so it is expanded without it; `path` is read at
/// :877, after it; the rest are read once `RSYNC_MODULE_PATH` is set at :920.
/// Called before authentication with `user_name` unset, which expands only
/// `secrets file`, and again after it to expand the rest.
///
/// upstream: `loadparm.c:333` `RETURN_EXPANDED` expands each string parameter
/// on its first read.
fn expand_module_vars(module: &mut ModuleDefinition, vars: &HookVariables<'_>) {
    let expand_path = |path: &Path, vars: &HookVariables<'_>| {
        PathBuf::from(expand_config_vars(&path.display().to_string(), vars))
    };
    if vars.user_name.is_none() {
        if let Some(path) = module.secrets_file.as_deref() {
            module.secrets_file = Some(expand_path(path, vars));
        }
        return;
    }

    let path_vars = HookVariables {
        module_path: None,
        ..*vars
    };
    module.path = expand_path(&module.path, &path_vars);

    let module_path = module.path.display().to_string();
    let later_vars = HookVariables {
        module_path: Some(&module_path),
        ..*vars
    };
    if let Some(dir) = module.temp_dir.as_deref() {
        module.temp_dir = Some(expand_config_vars(dir, &later_vars));
    }
    if let Some(path) = module.log_file.as_deref() {
        module.log_file = Some(expand_path(path, &later_vars));
    }
    if let Some(path) = module.exclude_from.as_deref() {
        module.exclude_from = Some(expand_path(path, &later_vars));
    }
    if let Some(path) = module.include_from.as_deref() {
        module.include_from = Some(expand_path(path, &later_vars));
    }
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
    /// The token as written in the template, e.g. `%RSYNC_USER_NAME%`.
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

/// Splits a leading `NAME%` off `rest` when it is a variable reference.
///
/// upstream: loadparm.c:250-252 - a reference is `%`, an uppercase letter,
/// then anything up to the next `%`; the whole name is handed to `getenv`.
fn delimited_variable(rest: &str) -> Option<(&str, &str)> {
    if !rest.as_bytes().first()?.is_ascii_uppercase() {
        return None;
    }
    let end = rest[1..].find('%')? + 1;
    Some((&rest[..end], &rest[end + 1..]))
}

/// Resolves a `%NAME%` reference.
///
/// A connection variable that has been set comes from `vars`; anything else is
/// a plain `getenv`, as upstream. `RSYNC_PID`, `RSYNC_REQUEST` and `RSYNC_ARG#`
/// are therefore left verbatim in the hook directives on both implementations:
/// upstream sets them only after the directives were read (clientserver.c:962,
/// :568).
fn resolve_hook_variable(name: &str, vars: &HookVariables<'_>) -> Option<String> {
    let set = match name {
        "RSYNC_MODULE_NAME" => vars.module_name,
        "RSYNC_MODULE_PATH" => vars.module_path,
        "RSYNC_HOST_NAME" => vars.host_name,
        "RSYNC_HOST_ADDR" => vars.host_addr,
        "RSYNC_USER_NAME" => vars.user_name,
        _ => None,
    };
    set.map(str::to_owned).or_else(|| std::env::var(name).ok())
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
/// upstream: `loadparm.c:237-325` `expand_vars(str, shell_escape=1)`, reached
/// via `FN_LOCAL_STRING_SHELL` for `early exec`, `name converter`,
/// `post-xfer exec` and `pre-xfer exec` (`daemon-parm.h:349/363/365/366`).
fn expand_exec_command(
    command: &str,
    vars: &HookVariables<'_>,
) -> Result<String, ShellHookRefusal> {
    expand_vars(command, vars, true)
}

/// Upstream's `expand_vars()`.
///
/// Only a `%NAME%` reference is expanded - a `%`, an uppercase letter, then
/// anything up to the next `%` - and only when the name resolves. Every other
/// `%` is literal, so `date +%m` and `100%%` pass through unchanged. With
/// `shell` set, a value under an `RSYNC_` name is escaped for the quoting
/// context it lands in, and refused outright when it carries a character a
/// shell could act on.
///
/// upstream: `loadparm.c:237-325`.
fn expand_vars(
    template: &str,
    vars: &HookVariables<'_>,
    shell: bool,
) -> Result<String, ShellHookRefusal> {
    let mut out = String::with_capacity(template.len());
    let mut scanner = ShellQuoteScanner::default();
    let mut rest = template;

    while let Some(pos) = rest.find('%') {
        push_literal(&mut out, &mut scanner, &rest[..pos]);
        rest = &rest[pos + 1..];

        let resolved = delimited_variable(rest)
            .and_then(|(name, tail)| Some((name, tail, resolve_hook_variable(name, vars)?)));
        match resolved {
            Some((name, tail, value)) => {
                out.push_str(&substitute_hook_value(
                    &format!("%{name}%"),
                    shell && name.starts_with("RSYNC_"),
                    &value,
                    scanner.context(),
                )?);
                rest = tail;
            }
            // upstream: loadparm.c:311-312 - an unresolved `%` is copied and the
            // scan resumes at the very next byte, so `%%RSYNC_X%` still
            // expands the reference after the first `%`.
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

#[cfg(test)]
mod variable_expansion_tests {
    use super::*;

    fn sample_hook_vars<'a>() -> HookVariables<'a> {
        HookVariables {
            module_path: Some("/srv/backup"),
            module_name: Some("backup"),
            user_name: Some("alice"),
            host_addr: Some("192.168.1.100"),
            host_name: Some("client.example.com"),
        }
    }

    /// The only `%` forms upstream expands are `%NAME%` references; the old oc
    /// built-ins `%MODULE%`, `%ADDR%` and `%DIFFHOST%` are plain names that
    /// `getenv` does not find. Measured on 3.5.1: `path = .../%MODULE%` serves
    /// the directory literally named `%MODULE%`.
    #[test]
    fn config_vars_have_no_built_in_names() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _module = crate::test_env::EnvGuard::remove("MODULE");
        let _addr = crate::test_env::EnvGuard::remove("ADDR");
        let _host = crate::test_env::EnvGuard::remove("DIFFHOST");
        let vars = sample_hook_vars();
        assert_eq!(
            expand_config_vars("/srv/%MODULE%/%ADDR%/%DIFFHOST%", &vars),
            "/srv/%MODULE%/%ADDR%/%DIFFHOST%"
        );
    }

    #[test]
    fn config_vars_expand_rsync_names_verbatim() {
        let vars = HookVariables {
            user_name: Some("a b"),
            ..sample_hook_vars()
        };
        // Not a shell hook: a value is substituted as-is, never escaped or
        // refused. upstream: loadparm.c:266 gates both on `shell_escape`.
        assert_eq!(
            expand_config_vars("/home/%RSYNC_USER_NAME%/%RSYNC_MODULE_NAME%", &vars),
            "/home/a b/backup"
        );
    }

    #[test]
    fn config_vars_keep_every_other_percent() {
        let vars = sample_hook_vars();
        for literal in ["100%%", "/p/%%/d", "%m %P %u", "/trailing%", "%lower%"] {
            assert_eq!(expand_config_vars(literal, &vars), literal);
        }
    }

    #[test]
    fn config_vars_fall_back_to_the_environment() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _set = crate::test_env::EnvGuard::set(
            "OC_RSYNC_TEST_EXPAND_SET",
            std::ffi::OsStr::new("value"),
        );
        let vars = sample_hook_vars();
        assert_eq!(
            expand_config_vars("/x/%OC_RSYNC_TEST_EXPAND_SET%", &vars),
            "/x/value"
        );
    }

    /// upstream: loadparm.c:250-312 - an unresolved `%FOO%` copies only its
    /// first `%` and resumes at the next byte, so the `%` that closed `FOO` can
    /// open the next reference.
    #[test]
    fn config_vars_resume_after_an_unresolved_name() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _unset = crate::test_env::EnvGuard::remove("OC_RSYNC_TEST_FOO");
        let _set = crate::test_env::EnvGuard::set("OC_RSYNC_TEST_BAR", std::ffi::OsStr::new("x"));
        let vars = sample_hook_vars();
        assert_eq!(
            expand_config_vars("%OC_RSYNC_TEST_FOO%OC_RSYNC_TEST_BAR%", &vars),
            "%OC_RSYNC_TEST_FOOx"
        );
    }

    fn module_with(path: &str) -> ModuleDefinition {
        ModuleDefinition {
            name: "backup".to_owned(),
            path: PathBuf::from(path),
            temp_dir: Some("%RSYNC_MODULE_PATH%/tmp".to_owned()),
            secrets_file: Some(PathBuf::from(
                "/etc/%RSYNC_HOST_ADDR%-%RSYNC_USER_NAME%.secrets",
            )),
            include_from: Some(PathBuf::from("%RSYNC_MODULE_PATH%/inc")),
            ..Default::default()
        }
    }

    /// Each parameter sees only the variables upstream has set when it reads
    /// it: `secrets file` (clientserver.c:809) precedes `RSYNC_USER_NAME`
    /// (:815), `path` (:877) precedes `RSYNC_MODULE_PATH` (:920), and the rest
    /// follow both.
    #[test]
    fn module_vars_follow_upstream_set_env_order() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _user = crate::test_env::EnvGuard::remove("RSYNC_USER_NAME");
        let _path = crate::test_env::EnvGuard::remove("RSYNC_MODULE_PATH");
        let mut module = module_with("/srv/%RSYNC_USER_NAME%/%RSYNC_MODULE_PATH%");

        let pre_auth = HookVariables {
            user_name: None,
            module_path: None,
            ..sample_hook_vars()
        };
        expand_module_vars(&mut module, &pre_auth);
        assert_eq!(
            module.secrets_file,
            Some(PathBuf::from(
                "/etc/192.168.1.100-%RSYNC_USER_NAME%.secrets"
            ))
        );
        assert_eq!(
            module.path,
            PathBuf::from("/srv/%RSYNC_USER_NAME%/%RSYNC_MODULE_PATH%"),
            "nothing but the secrets file is read before auth"
        );

        expand_module_vars(&mut module, &sample_hook_vars());
        assert_eq!(module.path, PathBuf::from("/srv/alice/%RSYNC_MODULE_PATH%"));
        assert_eq!(
            module.temp_dir.as_deref(),
            Some("/srv/alice/%RSYNC_MODULE_PATH%/tmp")
        );
        assert_eq!(
            module.include_from,
            Some(PathBuf::from("/srv/alice/%RSYNC_MODULE_PATH%/inc"))
        );
    }

    fn expanded(command: &str, vars: &HookVariables<'_>) -> String {
        expand_exec_command(command, vars).expect("template must expand")
    }

    /// upstream: loadparm.c:250 expands only `%NAME%` with an uppercase first
    /// letter. Measured on 3.5.1: `pre-xfer exec = echo "%m %P %u %a %h %p"`
    /// runs with every token literal.
    #[test]
    fn exec_command_leaves_single_character_escapes_literal() {
        let vars = sample_hook_vars();
        assert_eq!(
            expanded("date +%m %P %u %a %h %p", &vars),
            "date +%m %P %u %a %h %p"
        );
    }

    #[test]
    fn exec_command_escapes_rsync_values() {
        let vars = sample_hook_vars();
        assert_eq!(
            expanded(
                "notify --module=%RSYNC_MODULE_NAME% --user=%RSYNC_USER_NAME% --host=%RSYNC_HOST_NAME%",
                &vars
            ),
            "notify --module='backup' --user='alice' --host='client.example.com'"
        );
    }

    /// upstream sets RSYNC_PID (clientserver.c:962) and RSYNC_REQUEST
    /// (clientserver.c:568) only after the directives were expanded, so the
    /// references stay literal. Measured on 3.5.1.
    #[test]
    fn exec_command_leaves_later_set_variables_verbatim() {
        let vars = sample_hook_vars();
        assert_eq!(
            expanded("echo %RSYNC_PID% %RSYNC_REQUEST%", &vars),
            "echo %RSYNC_PID% %RSYNC_REQUEST%"
        );
    }

    #[test]
    fn exec_command_leaves_unresolved_reference_verbatim() {
        let vars = sample_hook_vars();
        assert_eq!(
            expanded("echo %OC_RSYNC_ABSENT_VARIABLE%", &vars),
            "echo %OC_RSYNC_ABSENT_VARIABLE%"
        );
    }

    /// upstream: loadparm.c:250-252 takes everything after the uppercase first
    /// letter up to the next `%` as the name, and a `%` that starts no
    /// reference is literal with the scan resuming at the next byte.
    #[test]
    fn exec_command_reads_names_the_way_upstream_does() {
        let vars = sample_hook_vars();
        assert_eq!(
            expanded("x %%RSYNC_MODULE_NAME% %a%RSYNC_MODULE_NAME%", &vars),
            "x %'backup' %a'backup'"
        );
        assert_eq!(expanded("%Oc-Absent Name%", &vars), "%Oc-Absent Name%");
    }

    #[test]
    fn exec_command_does_not_reread_a_substituted_value() {
        let vars = HookVariables {
            module_name: Some("%RSYNC_USER_NAME%"),
            ..sample_hook_vars()
        };
        assert_eq!(
            expanded("echo %RSYNC_MODULE_NAME%", &vars),
            "echo '%RSYNC_USER_NAME%'"
        );
    }

    #[test]
    fn exec_command_omits_the_wrap_inside_a_single_quoted_run() {
        let vars = sample_hook_vars();
        assert_eq!(expanded("echo '%RSYNC_USER_NAME%'", &vars), "echo 'alice'");
    }

    #[test]
    fn exec_command_refuses_before_the_double_quote_escape_can_apply() {
        let vars = HookVariables {
            user_name: Some("a$b"),
            ..sample_hook_vars()
        };
        // Every character the double-quoted arm backslash-escapes (`\ " ` $`)
        // is also in the refusal set, so a peer value never reaches that arm.
        // The arm is kept because upstream keeps it: `expand_vars_shell_escape`
        // is written for any value, not just the ones that survive
        // `shell_unsafe_value`. upstream: `loadparm.c:197-235`.
        assert!(expand_exec_command("echo \"%RSYNC_USER_NAME%\"", &vars).is_err());
    }

    #[test]
    fn exec_command_tracks_quote_context_across_a_closed_run() {
        let vars = sample_hook_vars();
        assert_eq!(
            expanded("echo 'x' %RSYNC_USER_NAME%", &vars),
            "echo 'x' 'alice'"
        );
    }

    #[test]
    fn exec_command_treats_a_literal_quote_inside_double_quotes_as_text() {
        let vars = sample_hook_vars();
        // The `'` inside `"..."` must not open a single-quoted run, or the
        // value after it would be escaped for the wrong context.
        // upstream: `loadparm.c:300-305`.
        assert_eq!(
            expanded("echo \"it's\" %RSYNC_USER_NAME%", &vars),
            "echo \"it's\" 'alice'"
        );
    }

    #[test]
    fn exec_command_refusal_names_the_delimited_token() {
        let vars = HookVariables {
            host_name: Some("a`id`b"),
            ..sample_hook_vars()
        };
        let refusal = expand_exec_command("notify %RSYNC_HOST_NAME%", &vars)
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
            let vars = HookVariables {
                user_name: Some(probe),
                ..sample_hook_vars()
            };
            assert!(
                expand_exec_command("notify %RSYNC_USER_NAME%", &vars).is_err(),
                "{probe:?} must be refused"
            );
        }
    }

    /// upstream: loadparm.c:266 - the check is keyed on the `RSYNC_` name, so
    /// even the operator's own module path is refused when it holds a space.
    /// Measured on 3.5.1: `path = /srv/mod x` with a hook naming
    /// `%RSYNC_MODULE_PATH%` drops the connection before `@RSYNCD: OK`.
    #[test]
    fn exec_command_refuses_a_module_path_holding_a_metacharacter() {
        let vars = HookVariables {
            module_path: Some("/srv/my backups"),
            ..sample_hook_vars()
        };
        assert!(expand_exec_command("du %RSYNC_MODULE_PATH%", &vars).is_err());
    }
}
