// Validation and finalization of `ModuleDefinitionBuilder`.
//
// Converts accumulated builder state into a validated `ModuleDefinition`,
// applying defaults for unset fields and enforcing cross-field constraints
// (e.g., `auth users` requires `secrets file`).

impl ModuleDefinitionBuilder {
    fn finish(
        self,
        config_path: &Path,
        default_secrets: Option<&Path>,
        default_incoming_chmod: Option<&str>,
        default_outgoing_chmod: Option<&str>,
        default_use_chroot: Option<bool>,
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

        Ok(ModuleDefinition {
            name: self.name,
            path,
            comment: self.comment,
            hosts_allow: self.hosts_allow.unwrap_or_default(),
            hosts_deny: self.hosts_deny.unwrap_or_default(),
            auth_users,
            secrets_file,
            bandwidth_limit: self.bandwidth_limit,
            bandwidth_limit_specified: self.bandwidth_limit_specified,
            bandwidth_burst: self.bandwidth_burst,
            bandwidth_burst_specified: self.bandwidth_burst_specified,
            bandwidth_limit_configured: self.bandwidth_limit_set,
            refuse_options: self.refuse_options.unwrap_or_default(),
            read_only: self.read_only.unwrap_or(true),
            write_only: self.write_only.unwrap_or(false),
            numeric_ids: self.numeric_ids.unwrap_or(false),
            uid: self.uid,
            gid: self.gid,
            timeout: self.timeout.unwrap_or(None),
            listable: self.listable.unwrap_or(true),
            use_chroot,
            max_connections: self.max_connections.unwrap_or(None),
            incoming_chmod: self
                .incoming_chmod
                .unwrap_or_else(|| default_incoming_chmod.map(str::to_string)),
            outgoing_chmod: self
                .outgoing_chmod
                .unwrap_or_else(|| default_outgoing_chmod.map(str::to_string)),
            fake_super: self.fake_super.unwrap_or(false),
            munge_symlinks: self.munge_symlinks.unwrap_or(None),
            max_verbosity: self.max_verbosity.unwrap_or(1),
            ignore_errors: self.ignore_errors.unwrap_or(false),
            ignore_nonreadable: self.ignore_nonreadable.unwrap_or(false),
            transfer_logging: self.transfer_logging.unwrap_or(false),
            log_format: self
                .log_format
                .unwrap_or_else(|| Some("%o %h [%a] %m (%u) %f %l".to_owned())),
            dont_compress: self.dont_compress.unwrap_or(None),
            early_exec: self.early_exec.unwrap_or(None),
            pre_xfer_exec: self.pre_xfer_exec.unwrap_or(None),
            post_xfer_exec: self.post_xfer_exec.unwrap_or(None),
            name_converter: self.name_converter.unwrap_or(None),
            temp_dir: self.temp_dir.unwrap_or(None),
            charset: self.charset.unwrap_or(None),
            forward_lookup: self.forward_lookup.unwrap_or(true),
            strict_modes: self.strict_modes.unwrap_or(true),
            exclude_from: self.exclude_from,
            include_from: self.include_from,
            open_noatime: self.open_noatime.unwrap_or(false),
            log_file: self.log_file,
            filter: self.filter,
            exclude: self.exclude,
            include: self.include,
        })
    }
}
