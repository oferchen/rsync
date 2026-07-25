// Per-directive setter methods for `ModuleDefinitionBuilder`.
//
// Every directive is last-wins: repeating it inside one module section
// overwrites the value the earlier occurrence stored. The single
// `last_wins_setters!` expansion below is the one place that assignment
// happens, so no directive can drift into a different duplicate policy.
//
// upstream: loadparm.c:379-470 do_parameter() - the parameter pointer is
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
    set_hosts_allow => hosts_allow: Vec<HostPattern>,
    set_hosts_deny => hosts_deny: Vec<HostPattern>,
    set_read_only => read_only: bool,
    set_write_only => write_only: bool,
    set_numeric_ids => numeric_ids: bool,
    set_listable => listable: bool,
    set_use_chroot => use_chroot: bool,
    set_uid => uid: u32,
    set_gid => gid: GidSetting,
    set_timeout => timeout: Option<NonZeroU64>,
    set_max_connections => max_connections: MaxConnections,
    set_incoming_chmod => incoming_chmod: Option<String>,
    set_outgoing_chmod => outgoing_chmod: Option<String>,
    set_fake_super => fake_super: bool,
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
    set_open_noatime => open_noatime: bool,
    set_log_file => log_file: PathBuf,
    /// Sets the per-module `reverse lookup` override.
    ///
    /// upstream: daemon-parm.h:78 `reverse_lookup` BOOL, P_LOCAL.
    set_reverse_lookup => reverse_lookup: bool,
    /// Sets the per-module `lock file` override.
    ///
    /// upstream: daemon-parm.h:46 `lock_file` STRING, P_LOCAL.
    set_lock_file => lock_file: PathBuf,
    /// Sets the per-module `syslog tag` override.
    ///
    /// upstream: loadparm.c syslog_tag (P_STRING, P_LOCAL, default "rsyncd").
    set_syslog_tag => syslog_tag: String,
    /// Sets the per-module `syslog facility` override to a canonical facility
    /// name.
    ///
    /// upstream: loadparm.c syslog_facility (P_ENUM, P_LOCAL, default LOG_DAEMON).
    set_syslog_facility => syslog_facility: String,
}

impl ModuleDefinitionBuilder {
    /// Stores the module `comment`, where an empty directive value means "no
    /// comment" and is represented by `None` rather than by an unset field.
    fn set_comment(&mut self, comment: Option<String>) {
        self.comment = comment;
    }

    /// Stores the `auth users` list, rejecting an empty one.
    ///
    /// upstream: authenticate.c:228 auth_server() treats a non-empty
    /// `lp_auth_users` as "authentication required", so an empty list is a
    /// configuration mistake rather than a way to disable authentication.
    fn set_auth_users(
        &mut self,
        users: Vec<AuthUser>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if users.is_empty() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        self.auth_users = Some(users);
        Ok(())
    }

    /// Stores the `secrets file` path after validating its ownership and mode.
    fn set_secrets_file(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        let validated = validate_secrets_file(&path, config_path, line)?;
        self.secrets_file = Some(validated);
        Ok(())
    }

    /// Stores the parsed `bwlimit` components.
    ///
    /// Unlike the generated setters this one writes several fields, including
    /// the `bandwidth_limit_set` flag `finish` reports as
    /// `bandwidth_limit_configured`.
    fn set_bandwidth_limit(
        &mut self,
        limit: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
    ) {
        self.bandwidth_limit = limit;
        self.bandwidth_burst = burst;
        self.bandwidth_burst_specified = burst_specified;
        self.bandwidth_limit_specified = true;
        self.bandwidth_limit_set = true;
    }

    /// Stores the `refuse options` list, rejecting an empty one.
    ///
    /// upstream: options.c:parse_refuse_options - the daemon builds its refusal
    /// table from a non-empty list; an empty directive would silently refuse
    /// nothing, which is always an operator mistake.
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
