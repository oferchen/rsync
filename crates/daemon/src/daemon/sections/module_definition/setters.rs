// Per-directive setter methods for `ModuleDefinitionBuilder`.
//
// Every directive is last-wins: repeating it inside one module section
// overwrites the value the earlier occurrence stored. The single
// `last_wins_setters!` expansion below is the one place that assignment
// happens, so no directive can drift into a different duplicate policy.
//
// upstream: loadparm.c:498 do_parameter() - the parameter pointer is
// resolved from parm_table and assigned unconditionally (`case P_INTEGER:
// *(int *)parm_ptr = atoi(parmvalue)`, `case P_STRING: string_set(parm_ptr,
// parmvalue)`). There is no seen-set and no duplicate check, so the last
// assignment wins.

/// Generates the per-directive setters that only record a value.
///
/// Each generated setter assigns unconditionally, giving every directive the
/// same last-wins semantics upstream's `do_parameter()` has. Setters that must
/// additionally validate their value are written out by hand below and keep
/// returning `Result`; only the duplicate rejection is gone from them.
macro_rules! last_wins_setters {
    ($( $(#[$attr:meta])* $setter:ident => $field:ident : $ty:ty ),+ $(,)?) => {
        impl ModuleDefinitionBuilder {
            $(
                $(#[$attr])*
                fn $setter(&mut self, value: $ty) {
                    self.$field = Some(value);
                }
            )+
        }
    };
}

last_wins_setters! {
    set_path => path: PathBuf,
    set_secrets_file => secrets_file: PathBuf,
    set_hosts_allow => hosts_allow: Vec<HostPattern>,
    set_hosts_deny => hosts_deny: Vec<HostPattern>,
    set_read_only => read_only: bool,
    set_write_only => write_only: bool,
    set_numeric_ids => numeric_ids: Option<bool>,
    set_listable => listable: bool,
    set_use_chroot => use_chroot: Option<bool>,
    set_uid => uid: Result<u32, String>,
    set_gid => gid: Result<GidSetting, String>,
    set_timeout => timeout: Option<NonZeroU64>,
    set_max_connections => max_connections: MaxConnections,
    set_incoming_chmod => incoming_chmod: Option<String>,
    set_outgoing_chmod => outgoing_chmod: Option<String>,
    set_fake_super => fake_super: bool,
    set_insecure_links => insecure_links: bool,
    set_munge_symlinks => munge_symlinks: Option<bool>,
    set_max_verbosity => max_verbosity: i32,
    set_ignore_errors => ignore_errors: bool,
    set_ignore_nonreadable => ignore_nonreadable: bool,
    set_transfer_logging => transfer_logging: bool,
    set_log_format => log_format: Option<String>,
    set_dont_compress => dont_compress: Option<String>,
    set_early_exec => early_exec: Option<String>,
    set_pre_xfer_exec => pre_xfer_exec: Option<String>,
    set_post_xfer_exec => post_xfer_exec: Option<String>,
    set_name_converter => name_converter: Option<String>,
    set_temp_dir => temp_dir: Option<String>,
    set_charset => charset: Option<String>,
    set_forward_lookup => forward_lookup: bool,
    set_strict_modes => strict_modes: bool,
    set_exclude_from => exclude_from: PathBuf,
    set_include_from => include_from: PathBuf,
    set_open_noatime => open_noatime: Option<bool>,
    set_log_file => log_file: PathBuf,
    /// Sets the per-module `reverse lookup` override.
    ///
    /// upstream: daemon-parm.txt:63 `reverse_lookup` BOOL, P_LOCAL.
    set_reverse_lookup => reverse_lookup: bool,
    /// Sets the per-module `lock file` override.
    ///
    /// upstream: daemon-parm.txt:34 `lock_file` STRING, P_LOCAL.
    set_lock_file => lock_file: PathBuf,
    /// Sets the per-module `syslog tag` override.
    ///
    /// upstream: daemon-parm.txt:44 `syslog_tag` STRING, P_LOCAL, default "rsyncd".
    set_syslog_tag => syslog_tag: String,
    /// Sets the per-module `syslog facility` override to a canonical facility
    /// name.
    ///
    /// upstream: daemon-parm.txt:54 `syslog_facility` ENUM, P_LOCAL, default LOG_DAEMON.
    set_syslog_facility => syslog_facility: String,
}

/// Generates the setters for the filter-rule slots, which hold `Vec<String>`
/// rather than `Option<String>`.
///
/// These three are the only P_LOCAL slots on the builder that are not
/// `Option`-shaped, so they are the only ones whose type can express
/// accumulation at all. That is exactly why they need saying out loud: for
/// every `Option` field above, "a repeat replaces" is true by construction
/// and no code has to enforce it, while here it has to be written down.
///
/// The `Vec` holds ONE entry, the directive's whole value with its whitespace
/// intact, rather than one entry per directive occurrence. Upstream keeps a
/// single string per slot and `string_set()` frees the previous one
/// (loadparm.c:382 string_set(), loadparm.c:571-572 do_parameter()), so a
/// second occurrence replaces the first rather than extending it. Splitting a
/// value into individual rules happens later, at rule-parse time, not here.
macro_rules! last_wins_rule_setters {
    ($( $(#[$attr:meta])* $setter:ident => $field:ident ),+ $(,)?) => {
        impl ModuleDefinitionBuilder {
            $(
                $(#[$attr])*
                fn $setter(&mut self, value: Vec<String>) {
                    self.$field = value;
                }
            )+
        }
    };
}

last_wins_rule_setters! {
    /// Sets the module's `filter` rules.
    ///
    /// upstream: daemon-parm.txt:27 `filter` STRING, P_LOCAL.
    set_filter => filter,
    /// Sets the module's `exclude` rules.
    ///
    /// upstream: daemon-parm.txt:25 `exclude` STRING, P_LOCAL.
    set_exclude => exclude,
    /// Sets the module's `include` rules.
    ///
    /// upstream: daemon-parm.txt:31 `include` STRING, P_LOCAL.
    set_include => include,
}

impl ModuleDefinitionBuilder {
    /// Stores the module `comment`, where an empty directive value means "no
    /// comment" and is represented by `None` rather than by an unset field.
    fn set_comment(&mut self, comment: Option<String>) {
        self.comment = comment;
    }

    /// Stores the `auth users` list. An empty list is an explicit "no
    /// restriction" that also overrides an inherited global list.
    ///
    /// upstream: authenticate.c:302-303 auth_server() - `if (!users || !*users)
    /// return "";` lets anyone in.
    fn set_auth_users(&mut self, users: Vec<AuthUser>) {
        self.auth_users = Some(users);
    }

    /// Stores the module's `auth digest` floor.
    ///
    /// Infallible by design: upstream accepts any string here (STRING,
    /// daemon-parm.txt:19) and only discovers an unsupported name when a client
    /// connects, refusing that one module rather than the daemon
    /// (authenticate.c:313-323 auth_server()).
    fn set_auth_digest(&mut self, digest: &str) {
        self.auth_digest = normalize_auth_digest(digest);
    }

    /// Stores the `refuse options` list, rejecting a value that parsed to no
    /// options.
    ///
    /// An empty directive value never reaches this setter: the caller stores an
    /// empty list for it, which refuses nothing like upstream's empty P_STRING
    /// (options.c:1016 set_refuse_options()).
    fn set_refuse_options(
        &mut self,
        options: Vec<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if options.is_empty() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "'refuse options' directive in module '{}' must list at least one option",
                    self.name
                ),
            ));
        }

        self.refuse_options = Some(options);
        Ok(())
    }
}
