// Validation and finalization of `ModuleDefinitionBuilder`.
//
// Converts accumulated builder state into a validated `ModuleDefinition`,
// applying defaults for unset fields and enforcing cross-field constraints
// (e.g., `auth users` requires `secrets file`).

impl ModuleDefinitionBuilder {
    /// Converts the accumulated builder state into a validated `ModuleDefinition`.
    ///
    /// Parameters not explicitly set on the module inherit from `defaults`,
    /// which captures P_LOCAL directives from the global section of rsyncd.conf.
    ///
    /// upstream: loadparm.c - `init_section()` copies the current global
    /// defaults (`Vars.l`) into each new module section. Per-module directives
    /// then override specific fields.
    fn finish(
        self,
        config_path: &Path,
        default_secrets: Option<&Path>,
        default_incoming_chmod: Option<&str>,
        default_outgoing_chmod: Option<&str>,
        default_use_chroot: Option<bool>,
        defaults: &GlobalModuleDefaults,
    ) -> Result<ModuleDefinition, DaemonError> {
        let path = self.path.ok_or_else(|| {
            config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' is missing required 'path' directive",
                    self.name
                ),
            )
        })?;

        let use_chroot = self.use_chroot.or(default_use_chroot).unwrap_or(true);

        // Windows has no chroot(2), so the absolute-path enforcement gated on
        // `use chroot` does not apply there. The check uses `Path::is_absolute()`
        // which rejects Unix-style paths (e.g. `/srv/docs`) on Windows for lack
        // of a drive letter. Mirrors the sibling fix in `module_parsing.rs`.
        //
        // The bare root `/` is `is_absolute()` and is accepted intentionally:
        // upstream loadparm.c (P_PATH) preserves a single-slash module path
        // verbatim, and clientserver.c serves from it both with and without
        // chroot (the chroot("/") path is a no-op). See the upstream
        // daemon-path-root-read scenario.
        #[cfg(unix)]
        if use_chroot && !path.is_absolute() {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' requires an absolute path when 'use chroot' is enabled",
                    self.name
                ),
            ));
        }

        if self.auth_users.as_ref().is_some_and(Vec::is_empty) {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        let auth_users = self.auth_users.unwrap_or_default();
        let secrets_file = if auth_users.is_empty() {
            self.secrets_file
        } else if let Some(path) = self.secrets_file {
            Some(path)
        } else if let Some(default) = default_secrets {
            Some(default.to_path_buf())
        } else {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' specifies 'auth users' but is missing the required 'secrets file' directive",
                    self.name
                ),
            ));
        };

        // upstream: loadparm.c - exclude/include/filter are STRING parameters.
        // In the global section they set defaults; per-module directives override
        // (not append to) the defaults. If the module has its own exclude rules,
        // use those; otherwise inherit the global defaults.
        let exclude = if self.exclude.is_empty() {
            defaults.exclude.clone()
        } else {
            self.exclude
        };
        let include = if self.include.is_empty() {
            defaults.include.clone()
        } else {
            self.include
        };
        let filter = if self.filter.is_empty() {
            defaults.filter.clone()
        } else {
            self.filter
        };

        // upstream: loadparm.c - hosts_allow/hosts_deny are STRING P_LOCAL
        // parameters with global defaults.
        let hosts_allow = self.hosts_allow.or_else(|| defaults.hosts_allow.clone()).unwrap_or_default();
        let hosts_deny = self.hosts_deny.or_else(|| defaults.hosts_deny.clone()).unwrap_or_default();

        Ok(ModuleDefinition {
            name: self.name,
            path,
            comment: self.comment.or_else(|| defaults.comment.clone()),
            hosts_allow,
            hosts_deny,
            auth_users,
            secrets_file,
            bandwidth_limit: self.bandwidth_limit,
            bandwidth_limit_specified: self.bandwidth_limit_specified,
            bandwidth_burst: self.bandwidth_burst,
            bandwidth_burst_specified: self.bandwidth_burst_specified,
            bandwidth_limit_configured: self.bandwidth_limit_set,
            refuse_options: self.refuse_options.unwrap_or_default(),
            read_only: self.read_only.or(defaults.read_only).unwrap_or(true),
            write_only: self.write_only.or(defaults.write_only).unwrap_or(false),
            numeric_ids: self.numeric_ids.or(defaults.numeric_ids).unwrap_or(false),
            uid: self.uid,
            gid: self.gid,
            timeout: self.timeout.or(defaults.timeout).unwrap_or(None),
            listable: self.listable.or(defaults.listable).unwrap_or(true),
            use_chroot,
            max_connections: self.max_connections.or(defaults.max_connections).unwrap_or(None),
            incoming_chmod: self
                .incoming_chmod
                .unwrap_or_else(|| default_incoming_chmod.map(str::to_string)),
            outgoing_chmod: self
                .outgoing_chmod
                .unwrap_or_else(|| default_outgoing_chmod.map(str::to_string)),
            fake_super: self.fake_super.or(defaults.fake_super).unwrap_or(false),
            munge_symlinks: self.munge_symlinks.or(defaults.munge_symlinks).unwrap_or(None),
            max_verbosity: self.max_verbosity.or(defaults.max_verbosity).unwrap_or(1),
            ignore_errors: self.ignore_errors.or(defaults.ignore_errors).unwrap_or(false),
            ignore_nonreadable: self.ignore_nonreadable.or(defaults.ignore_nonreadable).unwrap_or(false),
            transfer_logging: self.transfer_logging.or(defaults.transfer_logging).unwrap_or(false),
            log_format: self
                .log_format
                .unwrap_or_else(|| {
                    defaults.log_format.clone()
                        .or_else(|| Some("%o %h [%a] %m (%u) %f %l".to_owned()))
                }),
            log_file: self.log_file.or_else(|| defaults.log_file.clone()),
            dont_compress: self.dont_compress.unwrap_or_else(|| defaults.dont_compress.clone()),
            early_exec: self.early_exec.unwrap_or_else(|| defaults.early_exec.clone()),
            pre_xfer_exec: self.pre_xfer_exec.unwrap_or_else(|| defaults.pre_xfer_exec.clone()),
            post_xfer_exec: self.post_xfer_exec.unwrap_or_else(|| defaults.post_xfer_exec.clone()),
            name_converter: self.name_converter.unwrap_or_else(|| defaults.name_converter.clone()),
            temp_dir: self.temp_dir.unwrap_or_else(|| defaults.temp_dir.clone()),
            charset: self.charset.unwrap_or_else(|| defaults.charset.clone()),
            forward_lookup: self.forward_lookup.or(defaults.forward_lookup).unwrap_or(true),
            strict_modes: self.strict_modes.or(defaults.strict_modes).unwrap_or(true),
            exclude_from: self.exclude_from.or_else(|| defaults.exclude_from.clone()),
            include_from: self.include_from.or_else(|| defaults.include_from.clone()),
            open_noatime: self.open_noatime.or(defaults.open_noatime).unwrap_or(false),
            // upstream: daemon-parm.h:78 default True; module value overrides the
            // global-section default (defaults.reverse_lookup), else built-in True.
            reverse_lookup: self.reverse_lookup.or(defaults.reverse_lookup).unwrap_or(true),
            // upstream: daemon-parm.h:46 - a module `lock file` overrides the
            // daemon-wide lock file; `None` inherits it at connection setup.
            lock_file: self.lock_file,
            filter,
            exclude,
            include,
        })
    }
}
