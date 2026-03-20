impl RuntimeOptions {
    fn load_config_modules(
        &mut self,
        value: &OsString,
        seen_modules: &mut HashSet<String>,
    ) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let parsed = parse_config_modules(&path)?;

        // Retain the config path for SIGHUP reload. Only the first config
        // file loaded is reloadable; subsequent --config flags add modules
        // but the first path is the one re-read on SIGHUP (matching upstream
        // rsync behaviour where only the primary config is reloaded).
        if self.config_path.is_none() {
            self.config_path = Some(path.clone());
        }

        for (options, origin) in parsed.global_refuse_options {
            self.inherit_global_refuse_options(options, &origin)?;
        }

        if let Some((pid_file, origin)) = parsed.pid_file {
            self.set_config_pid_file(pid_file, &origin)?;
        }

        if let Some((reverse_lookup, origin)) = parsed.reverse_lookup {
            self.set_reverse_lookup_from_config(reverse_lookup, &origin)?;
        }

        if let Some((lock_file, origin)) = parsed.lock_file {
            self.set_config_lock_file(lock_file, &origin)?;
        }

        if let Some((components, _origin)) = parsed.global_bandwidth_limit
            && !self.bandwidth_limit_configured
        {
            self.bandwidth_limit = components.rate();
            self.bandwidth_burst = components.burst();
            self.bandwidth_limit_configured = true;
        }

        if let Some((secrets, origin)) = parsed.global_secrets_file {
            self.set_global_secrets_file(secrets, &origin)?;
        }

        if let Some((incoming, origin)) = parsed.global_incoming_chmod {
            self.set_global_incoming_chmod(incoming, &origin)?;
        }

        if let Some((outgoing, origin)) = parsed.global_outgoing_chmod {
            self.set_global_outgoing_chmod(outgoing, &origin)?;
        }

        if let Some((facility, origin)) = parsed.syslog_facility {
            self.set_syslog_facility_from_config(facility, &origin)?;
        }

        if let Some((tag, origin)) = parsed.syslog_tag {
            self.set_syslog_tag_from_config(tag, &origin)?;
        }

        // Apply the `address` directive only when no CLI --address/--bind was given.
        // upstream: clientserver.c - CLI --address overrides the config file `address`.
        if let Some((addr, _origin)) = parsed.bind_address {
            if !self.bind_address_overridden {
                self.bind_address = addr;
                self.bind_address_overridden = true;
                self.address_family = Some(AddressFamily::from_ip(addr));
            }
        }

        if let Some((uid_str, origin)) = parsed.daemon_uid {
            self.set_daemon_uid_from_config(&uid_str, &origin)?;
        }

        if let Some((gid_str, origin)) = parsed.daemon_gid {
            self.set_daemon_gid_from_config(&gid_str, &origin)?;
        }

        if let Some((backlog, origin)) = parsed.listen_backlog {
            self.set_listen_backlog_from_config(backlog, &origin)?;
        }

        if let Some((port, _origin)) = parsed.rsync_port {
            self.rsync_port = Some(port);
        }

        if let Some((opts, origin)) = parsed.socket_options {
            self.set_socket_options_from_config(opts, &origin)?;
        }

        if let Some((enabled, _origin)) = parsed.proxy_protocol {
            self.proxy_protocol = enabled;
        }

        if let Some((chroot_path, _origin)) = parsed.daemon_chroot {
            self.daemon_chroot = Some(chroot_path);
        }

        if !parsed.motd_lines.is_empty() {
            self.motd_lines.extend(parsed.motd_lines);
        }

        let mut modules = parsed.modules;
        if let Some(global) = &self.global_refuse_options {
            for module in &mut modules {
                module.inherit_refuse_options(global);
            }
        }

        if let Some(incoming) = self.global_incoming_chmod.as_deref() {
            for module in &mut modules {
                module.inherit_incoming_chmod(Some(incoming));
            }
        }

        if let Some(outgoing) = self.global_outgoing_chmod.as_deref() {
            for module in &mut modules {
                module.inherit_outgoing_chmod(Some(outgoing));
            }
        }

        for module in modules {
            if !seen_modules.insert(module.name.clone()) {
                return Err(duplicate_module(&module.name));
            }
            self.modules.push(module);
        }

        Ok(())
    }

    fn inherit_global_refuse_options(
        &mut self,
        options: Vec<String>,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.global_refuse_options {
            if existing != &options {
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'refuse options' directive in global section",
                ));
            }
            return Ok(());
        }

        for module in &mut self.modules {
            module.inherit_refuse_options(&options);
        }

        self.global_refuse_options = Some(options);
        Ok(())
    }

    fn set_global_incoming_chmod(
        &mut self,
        value: String,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.global_incoming_chmod {
            if existing != &value {
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'incoming chmod' directive in global section",
                ));
            }
            return Ok(());
        }

        for module in &mut self.modules {
            module.inherit_incoming_chmod(Some(&value));
        }

        self.global_incoming_chmod = Some(value);
        Ok(())
    }

    fn set_global_outgoing_chmod(
        &mut self,
        value: String,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.global_outgoing_chmod {
            if existing != &value {
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'outgoing chmod' directive in global section",
                ));
            }
            return Ok(());
        }

        for module in &mut self.modules {
            module.inherit_outgoing_chmod(Some(&value));
        }

        self.global_outgoing_chmod = Some(value);
        Ok(())
    }

    fn set_syslog_facility_from_config(
        &mut self,
        value: String,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.syslog_facility {
            if self.syslog_facility_from_config {
                if existing == &value {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'syslog facility' directive in global section",
                ));
            }
            return Ok(());
        }

        self.syslog_facility = Some(value);
        self.syslog_facility_from_config = true;
        Ok(())
    }

    fn set_syslog_tag_from_config(
        &mut self,
        value: String,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.syslog_tag {
            if self.syslog_tag_from_config {
                if existing == &value {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'syslog tag' directive in global section",
                ));
            }
            return Ok(());
        }

        self.syslog_tag = Some(value);
        self.syslog_tag_from_config = true;
        Ok(())
    }

    fn set_listen_backlog_from_config(
        &mut self,
        value: u32,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = self.listen_backlog {
            if self.listen_backlog_from_config {
                if existing == value {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'listen backlog' directive in global section",
                ));
            }
            return Ok(());
        }

        self.listen_backlog = Some(value);
        self.listen_backlog_from_config = true;
        Ok(())
    }

    fn set_socket_options_from_config(
        &mut self,
        value: String,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.socket_options {
            if self.socket_options_from_config {
                if *existing == value {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'socket options' directive in global section",
                ));
            }
            return Ok(());
        }

        self.socket_options = Some(value);
        self.socket_options_from_config = true;
        Ok(())
    }

    fn set_config_pid_file(
        &mut self,
        path: PathBuf,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.pid_file {
            if self.pid_file_from_config {
                if existing == &path {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'pid file' directive in global section",
                ));
            }

            return Ok(());
        }

        self.pid_file = Some(path);
        self.pid_file_from_config = true;
        Ok(())
    }

    fn set_reverse_lookup_from_config(
        &mut self,
        value: bool,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if self.reverse_lookup_configured {
            return Err(config_parse_error(
                &origin.path,
                origin.line,
                "duplicate 'reverse lookup' directive in global section",
            ));
        }

        self.reverse_lookup = value;
        self.reverse_lookup_configured = true;
        Ok(())
    }

    fn set_config_lock_file(
        &mut self,
        path: PathBuf,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.lock_file {
            if self.lock_file_from_config {
                if existing == &path {
                    return Ok(());
                }
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'lock file' directive in global section",
                ));
            }

            return Ok(());
        }

        self.lock_file = Some(path);
        self.lock_file_from_config = true;
        Ok(())
    }

    fn set_global_secrets_file(
        &mut self,
        path: PathBuf,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if let Some(existing) = &self.global_secrets_file
            && self.global_secrets_from_config
        {
            if existing == &path {
                return Ok(());
            }

            return Err(config_parse_error(
                &origin.path,
                origin.line,
                "duplicate 'secrets file' directive in global section",
            ));
        }

        self.global_secrets_file = Some(path);
        self.global_secrets_from_config = true;
        Ok(())
    }

    /// Resolves a uid string (username or numeric) and stores it as the daemon uid.
    ///
    /// upstream: loadparm.c - global `uid` parameter accepts both numeric IDs and
    /// usernames. Resolution happens at config load time via `getpwnam_r`.
    fn set_daemon_uid_from_config(
        &mut self,
        value: &str,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if self.daemon_uid.is_some() {
            return Err(config_parse_error(
                &origin.path,
                origin.line,
                "duplicate 'uid' directive in global section",
            ));
        }

        let resolved =
            resolve_uid(value).map_err(|msg| config_parse_error(&origin.path, origin.line, msg))?;
        self.daemon_uid = Some(resolved);
        Ok(())
    }

    /// Resolves a gid string (groupname or numeric) and stores it as the daemon gid.
    ///
    /// upstream: loadparm.c - global `gid` parameter accepts both numeric IDs and
    /// groupnames. Resolution happens at config load time via `getgrnam_r`.
    fn set_daemon_gid_from_config(
        &mut self,
        value: &str,
        origin: &ConfigDirectiveOrigin,
    ) -> Result<(), DaemonError> {
        if self.daemon_gid.is_some() {
            return Err(config_parse_error(
                &origin.path,
                origin.line,
                "duplicate 'gid' directive in global section",
            ));
        }

        let resolved =
            resolve_gid(value).map_err(|msg| config_parse_error(&origin.path, origin.line, msg))?;
        self.daemon_gid = Some(resolved);
        Ok(())
    }
}
