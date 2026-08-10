// Per-directive setter methods for `ModuleDefinitionBuilder`.
//
// Each setter enforces duplicate-detection: calling the same setter twice
// within one module section produces a parse error. This mirrors upstream
// rsync's one-value-per-directive semantics.

impl ModuleDefinitionBuilder {
    fn set_path(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.path.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'path' directive in module '{}'", self.name),
            ));
        }

        self.path = Some(path);
        Ok(())
    }

    fn set_comment(
        &mut self,
        comment: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.comment.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'comment' directive in module '{}'", self.name),
            ));
        }

        self.comment = comment;
        Ok(())
    }

    fn set_hosts_allow(
        &mut self,
        patterns: Vec<HostPattern>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.hosts_allow.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'hosts allow' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.hosts_allow = Some(patterns);
        Ok(())
    }

    fn set_hosts_deny(
        &mut self,
        patterns: Vec<HostPattern>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.hosts_deny.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'hosts deny' directive in module '{}'", self.name),
            ));
        }

        self.hosts_deny = Some(patterns);
        Ok(())
    }

    fn set_auth_users(
        &mut self,
        users: Vec<AuthUser>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.auth_users.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'auth users' directive in module '{}'", self.name),
            ));
        }

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

    fn set_secrets_file(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.secrets_file.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'secrets file' directive in module '{}'",
                    self.name
                ),
            ));
        }

        let validated = validate_secrets_file(&path, config_path, line)?;
        self.secrets_file = Some(validated);
        Ok(())
    }

    fn set_bandwidth_limit(
        &mut self,
        limit: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.bandwidth_limit_set {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'bwlimit' directive in module '{}'", self.name),
            ));
        }

        self.bandwidth_limit = limit;
        self.bandwidth_burst = burst;
        self.bandwidth_burst_specified = burst_specified;
        self.bandwidth_limit_specified = true;
        self.bandwidth_limit_set = true;
        Ok(())
    }

    fn set_refuse_options(
        &mut self,
        options: Vec<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.refuse_options.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'refuse options' directive in module '{}'",
                    self.name
                ),
            ));
        }

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

    fn set_read_only(
        &mut self,
        read_only: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.read_only.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'read only' directive in module '{}'", self.name),
            ));
        }

        self.read_only = Some(read_only);
        Ok(())
    }

    fn set_write_only(
        &mut self,
        write_only: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.write_only.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'write only' directive in module '{}'", self.name),
            ));
        }

        self.write_only = Some(write_only);
        Ok(())
    }

    fn set_numeric_ids(
        &mut self,
        numeric_ids: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.numeric_ids.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'numeric ids' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.numeric_ids = Some(numeric_ids);
        Ok(())
    }

    fn set_listable(
        &mut self,
        listable: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.listable.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'list' directive in module '{}'", self.name),
            ));
        }

        self.listable = Some(listable);
        Ok(())
    }

    fn set_use_chroot(
        &mut self,
        use_chroot: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.use_chroot.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'use chroot' directive in module '{}'", self.name),
            ));
        }

        self.use_chroot = Some(use_chroot);
        Ok(())
    }

    fn set_uid(&mut self, uid: u32, config_path: &Path, line: usize) -> Result<(), DaemonError> {
        if self.uid.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'uid' directive in module '{}'", self.name),
            ));
        }

        self.uid = Some(uid);
        Ok(())
    }

    fn set_gid(&mut self, gid: u32, config_path: &Path, line: usize) -> Result<(), DaemonError> {
        if self.gid.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'gid' directive in module '{}'", self.name),
            ));
        }

        self.gid = Some(gid);
        Ok(())
    }

    fn set_timeout(
        &mut self,
        timeout: Option<NonZeroU64>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.timeout.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'timeout' directive in module '{}'", self.name),
            ));
        }

        self.timeout = Some(timeout);
        Ok(())
    }

    fn set_max_connections(
        &mut self,
        max: Option<NonZeroU32>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.max_connections.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'max connections' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.max_connections = Some(max);
        Ok(())
    }

    fn set_incoming_chmod(
        &mut self,
        chmod: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.incoming_chmod.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'incoming chmod' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.incoming_chmod = Some(chmod);
        Ok(())
    }

    fn set_outgoing_chmod(
        &mut self,
        chmod: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.outgoing_chmod.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'outgoing chmod' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.outgoing_chmod = Some(chmod);
        Ok(())
    }

    fn set_fake_super(
        &mut self,
        fake_super: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.fake_super.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'fake super' directive in module '{}'", self.name),
            ));
        }

        self.fake_super = Some(fake_super);
        Ok(())
    }

    fn set_munge_symlinks(
        &mut self,
        munge_symlinks: Option<bool>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.munge_symlinks.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'munge symlinks' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.munge_symlinks = Some(munge_symlinks);
        Ok(())
    }

    fn set_max_verbosity(
        &mut self,
        max_verbosity: i32,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.max_verbosity.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'max verbosity' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.max_verbosity = Some(max_verbosity);
        Ok(())
    }

    fn set_ignore_errors(
        &mut self,
        ignore_errors: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.ignore_errors.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'ignore errors' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.ignore_errors = Some(ignore_errors);
        Ok(())
    }

    fn set_ignore_nonreadable(
        &mut self,
        ignore_nonreadable: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.ignore_nonreadable.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'ignore nonreadable' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.ignore_nonreadable = Some(ignore_nonreadable);
        Ok(())
    }

    fn set_transfer_logging(
        &mut self,
        transfer_logging: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.transfer_logging.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'transfer logging' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.transfer_logging = Some(transfer_logging);
        Ok(())
    }

    fn set_log_format(
        &mut self,
        log_format: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.log_format.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'log format' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.log_format = Some(log_format);
        Ok(())
    }

    fn set_dont_compress(
        &mut self,
        dont_compress: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.dont_compress.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'dont compress' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.dont_compress = Some(dont_compress);
        Ok(())
    }

    fn set_early_exec(
        &mut self,
        early_exec: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.early_exec.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'early exec' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.early_exec = Some(early_exec);
        Ok(())
    }

    fn set_pre_xfer_exec(
        &mut self,
        pre_xfer_exec: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.pre_xfer_exec.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'pre-xfer exec' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.pre_xfer_exec = Some(pre_xfer_exec);
        Ok(())
    }

    fn set_post_xfer_exec(
        &mut self,
        post_xfer_exec: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.post_xfer_exec.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'post-xfer exec' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.post_xfer_exec = Some(post_xfer_exec);
        Ok(())
    }

    fn set_name_converter(
        &mut self,
        name_converter: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.name_converter.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'name converter' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.name_converter = Some(name_converter);
        Ok(())
    }

    fn set_temp_dir(
        &mut self,
        temp_dir: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.temp_dir.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'temp dir' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.temp_dir = Some(temp_dir);
        Ok(())
    }

    fn set_charset(
        &mut self,
        charset: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.charset.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'charset' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.charset = Some(charset);
        Ok(())
    }

    fn set_forward_lookup(
        &mut self,
        forward_lookup: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.forward_lookup.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'forward lookup' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.forward_lookup = Some(forward_lookup);
        Ok(())
    }

    fn set_strict_modes(
        &mut self,
        strict_modes: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.strict_modes.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'strict modes' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.strict_modes = Some(strict_modes);
        Ok(())
    }

    fn set_exclude_from(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.exclude_from.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'exclude from' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.exclude_from = Some(path);
        Ok(())
    }

    fn set_include_from(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.include_from.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'include from' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.include_from = Some(path);
        Ok(())
    }

    fn set_open_noatime(
        &mut self,
        open_noatime: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.open_noatime.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'open noatime' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.open_noatime = Some(open_noatime);
        Ok(())
    }

    fn set_log_file(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.log_file.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'log file' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.log_file = Some(path);
        Ok(())
    }
}
