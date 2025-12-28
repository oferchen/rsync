struct ModuleDefinitionBuilder {
    name: String,
    path: Option<PathBuf>,
    comment: Option<String>,
    hosts_allow: Option<Vec<HostPattern>>,
    hosts_deny: Option<Vec<HostPattern>>,
    auth_users: Option<Vec<String>>,
    secrets_file: Option<PathBuf>,
    declaration_line: usize,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_limit_specified: bool,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_burst_specified: bool,
    bandwidth_limit_set: bool,
    refuse_options: Option<Vec<String>>,
    read_only: Option<bool>,
    write_only: Option<bool>,
    numeric_ids: Option<bool>,
    uid: Option<u32>,
    gid: Option<u32>,
    timeout: Option<Option<NonZeroU64>>,
    listable: Option<bool>,
    use_chroot: Option<bool>,
    max_connections: Option<Option<NonZeroU32>>,
    incoming_chmod: Option<Option<String>>,
    outgoing_chmod: Option<Option<String>>,
}

impl ModuleDefinitionBuilder {
    fn new(name: String, line: usize) -> Self {
        Self {
            name,
            path: None,
            comment: None,
            hosts_allow: None,
            hosts_deny: None,
            auth_users: None,
            secrets_file: None,
            declaration_line: line,
            bandwidth_limit: None,
            bandwidth_limit_specified: false,
            bandwidth_burst: None,
            bandwidth_burst_specified: false,
            bandwidth_limit_set: false,
            refuse_options: None,
            read_only: None,
            write_only: None,
            numeric_ids: None,
            uid: None,
            gid: None,
            timeout: None,
            listable: None,
            use_chroot: None,
            max_connections: None,
            incoming_chmod: None,
            outgoing_chmod: None,
        }
    }

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
        users: Vec<String>,
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

    fn finish(
        self,
        config_path: &Path,
        default_secrets: Option<&Path>,
        default_incoming_chmod: Option<&str>,
        default_outgoing_chmod: Option<&str>,
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

        let use_chroot = self.use_chroot.unwrap_or(true);

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
        })
    }
}

#[cfg(test)]
mod module_definition_builder_tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config_path() -> PathBuf {
        PathBuf::from("/test/rsyncd.conf")
    }

    // ==================== new() tests ====================

    #[test]
    fn builder_new_sets_name_and_line() {
        let builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 42);
        assert_eq!(builder.name, "testmod");
        assert_eq!(builder.declaration_line, 42);
    }

    #[test]
    fn builder_new_starts_with_all_none() {
        let builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        assert!(builder.path.is_none());
        assert!(builder.comment.is_none());
        assert!(builder.hosts_allow.is_none());
        assert!(builder.hosts_deny.is_none());
        assert!(builder.auth_users.is_none());
        assert!(builder.secrets_file.is_none());
        assert!(builder.bandwidth_limit.is_none());
        assert!(builder.refuse_options.is_none());
        assert!(builder.read_only.is_none());
        assert!(builder.write_only.is_none());
        assert!(builder.numeric_ids.is_none());
        assert!(builder.uid.is_none());
        assert!(builder.gid.is_none());
        assert!(builder.timeout.is_none());
        assert!(builder.listable.is_none());
        assert!(builder.use_chroot.is_none());
        assert!(builder.max_connections.is_none());
        assert!(builder.incoming_chmod.is_none());
        assert!(builder.outgoing_chmod.is_none());
    }

    // ==================== set_path tests ====================

    #[test]
    fn set_path_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_path(PathBuf::from("/data"), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.path, Some(PathBuf::from("/data")));
    }

    #[test]
    fn set_path_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 5).unwrap();
        let result = builder.set_path(PathBuf::from("/other"), &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== set_comment tests ====================

    #[test]
    fn set_comment_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_comment(Some("A test module".to_owned()), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.comment, Some("A test module".to_owned()));
    }

    #[test]
    fn set_comment_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_comment(None, &test_config_path(), 5);
        assert!(result.is_ok());
        assert!(builder.comment.is_none());
    }

    #[test]
    fn set_comment_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_comment(Some("first".to_owned()), &test_config_path(), 5).unwrap();
        let result = builder.set_comment(Some("second".to_owned()), &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== set_hosts_allow tests ====================

    #[test]
    fn set_hosts_allow_stores_patterns() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let patterns = vec![HostPattern::Any];
        let result = builder.set_hosts_allow(patterns.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.hosts_allow, Some(patterns));
    }

    #[test]
    fn set_hosts_allow_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_hosts_allow(vec![HostPattern::Any], &test_config_path(), 5).unwrap();
        let result = builder.set_hosts_allow(vec![], &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== set_hosts_deny tests ====================

    #[test]
    fn set_hosts_deny_stores_patterns() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let patterns = vec![HostPattern::Any];
        let result = builder.set_hosts_deny(patterns.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.hosts_deny, Some(patterns));
    }

    #[test]
    fn set_hosts_deny_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_hosts_deny(vec![HostPattern::Any], &test_config_path(), 5).unwrap();
        let result = builder.set_hosts_deny(vec![], &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== set_auth_users tests ====================

    #[test]
    fn set_auth_users_stores_users() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        let result = builder.set_auth_users(users.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.auth_users, Some(users));
    }

    #[test]
    fn set_auth_users_rejects_empty() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_auth_users(vec![], &test_config_path(), 5);
        assert!(result.is_err());
    }

    #[test]
    fn set_auth_users_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_auth_users(vec!["alice".to_owned()], &test_config_path(), 5).unwrap();
        let result = builder.set_auth_users(vec!["bob".to_owned()], &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== set_bandwidth_limit tests ====================

    #[test]
    fn set_bandwidth_limit_stores_values() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let limit = NonZeroU64::new(1000);
        let burst = NonZeroU64::new(2000);
        let result = builder.set_bandwidth_limit(limit, burst, true, &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.bandwidth_limit, limit);
        assert_eq!(builder.bandwidth_burst, burst);
        assert!(builder.bandwidth_burst_specified);
        assert!(builder.bandwidth_limit_specified);
        assert!(builder.bandwidth_limit_set);
    }

    #[test]
    fn set_bandwidth_limit_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_bandwidth_limit(None, None, false, &test_config_path(), 5).unwrap();
        let result = builder.set_bandwidth_limit(NonZeroU64::new(100), None, false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== set_refuse_options tests ====================

    #[test]
    fn set_refuse_options_stores_options() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let options = vec!["delete".to_owned(), "hardlinks".to_owned()];
        let result = builder.set_refuse_options(options.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.refuse_options, Some(options));
    }

    #[test]
    fn set_refuse_options_rejects_empty() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_refuse_options(vec![], &test_config_path(), 5);
        assert!(result.is_err());
    }

    #[test]
    fn set_refuse_options_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_refuse_options(vec!["delete".to_owned()], &test_config_path(), 5).unwrap();
        let result = builder.set_refuse_options(vec!["hardlinks".to_owned()], &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== Boolean setter tests ====================

    #[test]
    fn set_read_only_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_read_only(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.read_only, Some(false));
    }

    #[test]
    fn set_read_only_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_read_only(true, &test_config_path(), 5).unwrap();
        let result = builder.set_read_only(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_write_only_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_write_only(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.write_only, Some(true));
    }

    #[test]
    fn set_write_only_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_write_only(true, &test_config_path(), 5).unwrap();
        let result = builder.set_write_only(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_numeric_ids_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_numeric_ids(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.numeric_ids, Some(true));
    }

    #[test]
    fn set_numeric_ids_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_numeric_ids(true, &test_config_path(), 5).unwrap();
        let result = builder.set_numeric_ids(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_listable_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_listable(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.listable, Some(false));
    }

    #[test]
    fn set_listable_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_listable(true, &test_config_path(), 5).unwrap();
        let result = builder.set_listable(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_use_chroot_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_use_chroot(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.use_chroot, Some(false));
    }

    #[test]
    fn set_use_chroot_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_use_chroot(true, &test_config_path(), 5).unwrap();
        let result = builder.set_use_chroot(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== Numeric setter tests ====================

    #[test]
    fn set_uid_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_uid(1000, &test_config_path(), 5).unwrap();
        assert_eq!(builder.uid, Some(1000));
    }

    #[test]
    fn set_uid_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_uid(1000, &test_config_path(), 5).unwrap();
        let result = builder.set_uid(2000, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_gid_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_gid(100, &test_config_path(), 5).unwrap();
        assert_eq!(builder.gid, Some(100));
    }

    #[test]
    fn set_gid_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_gid(100, &test_config_path(), 5).unwrap();
        let result = builder.set_gid(200, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_timeout_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let timeout = NonZeroU64::new(60);
        builder.set_timeout(timeout, &test_config_path(), 5).unwrap();
        assert_eq!(builder.timeout, Some(timeout));
    }

    #[test]
    fn set_timeout_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_timeout(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.timeout, Some(None));
    }

    #[test]
    fn set_timeout_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_timeout(NonZeroU64::new(60), &test_config_path(), 5).unwrap();
        let result = builder.set_timeout(NonZeroU64::new(120), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_max_connections_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let max = NonZeroU32::new(10);
        builder.set_max_connections(max, &test_config_path(), 5).unwrap();
        assert_eq!(builder.max_connections, Some(max));
    }

    #[test]
    fn set_max_connections_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_max_connections(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.max_connections, Some(None));
    }

    #[test]
    fn set_max_connections_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_max_connections(NonZeroU32::new(10), &test_config_path(), 5).unwrap();
        let result = builder.set_max_connections(NonZeroU32::new(20), &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== chmod setter tests ====================

    #[test]
    fn set_incoming_chmod_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_incoming_chmod(Some("Dg+s,ug+w".to_owned()), &test_config_path(), 5).unwrap();
        assert_eq!(builder.incoming_chmod, Some(Some("Dg+s,ug+w".to_owned())));
    }

    #[test]
    fn set_incoming_chmod_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_incoming_chmod(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.incoming_chmod, Some(None));
    }

    #[test]
    fn set_incoming_chmod_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_incoming_chmod(Some("a+r".to_owned()), &test_config_path(), 5).unwrap();
        let result = builder.set_incoming_chmod(Some("a+w".to_owned()), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_outgoing_chmod_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_outgoing_chmod(Some("Fo-w,+X".to_owned()), &test_config_path(), 5).unwrap();
        assert_eq!(builder.outgoing_chmod, Some(Some("Fo-w,+X".to_owned())));
    }

    #[test]
    fn set_outgoing_chmod_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_outgoing_chmod(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.outgoing_chmod, Some(None));
    }

    #[test]
    fn set_outgoing_chmod_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_outgoing_chmod(Some("a+r".to_owned()), &test_config_path(), 5).unwrap();
        let result = builder.set_outgoing_chmod(Some("a+w".to_owned()), &test_config_path(), 10);
        assert!(result.is_err());
    }

    // ==================== finish() tests ====================

    #[test]
    fn finish_succeeds_with_minimal_config() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.name, "testmod");
        assert_eq!(def.path, PathBuf::from("/data"));
        assert!(def.read_only); // default
        assert!(!def.write_only); // default
        assert!(def.listable); // default
        assert!(def.use_chroot); // default
    }

    #[test]
    fn finish_fails_without_path() {
        let builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn finish_requires_absolute_path_with_chroot() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("relative/path"), &test_config_path(), 2).unwrap();
        // use_chroot defaults to true
        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn finish_allows_relative_path_without_chroot() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("relative/path"), &test_config_path(), 2).unwrap();
        builder.set_use_chroot(false, &test_config_path(), 3).unwrap();
        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn finish_applies_default_secrets_for_auth_users() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_auth_users(vec!["alice".to_owned()], &test_config_path(), 3).unwrap();
        let default_secrets = PathBuf::from("/etc/secrets");
        let result = builder.finish(&test_config_path(), Some(&default_secrets), None, None);
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.secrets_file, Some(PathBuf::from("/etc/secrets")));
    }

    #[test]
    fn finish_fails_auth_users_without_secrets() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_auth_users(vec!["alice".to_owned()], &test_config_path(), 3).unwrap();
        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn finish_applies_default_chmod_values() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        let result = builder.finish(
            &test_config_path(),
            None,
            Some("Dg+s"),
            Some("Fo-w"),
        );
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.incoming_chmod.as_deref(), Some("Dg+s"));
        assert_eq!(def.outgoing_chmod.as_deref(), Some("Fo-w"));
    }

    #[test]
    fn finish_preserves_explicit_chmod_over_defaults() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_incoming_chmod(Some("a+r".to_owned()), &test_config_path(), 3).unwrap();
        builder.set_outgoing_chmod(Some("a+x".to_owned()), &test_config_path(), 4).unwrap();
        let result = builder.finish(
            &test_config_path(),
            None,
            Some("default-in"),
            Some("default-out"),
        );
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.incoming_chmod.as_deref(), Some("a+r"));
        assert_eq!(def.outgoing_chmod.as_deref(), Some("a+x"));
    }

    #[test]
    fn finish_transfers_all_set_values() {
        let mut builder = ModuleDefinitionBuilder::new("fullmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/full/path"), &test_config_path(), 2).unwrap();
        builder.set_comment(Some("Full test".to_owned()), &test_config_path(), 3).unwrap();
        builder.set_read_only(false, &test_config_path(), 4).unwrap();
        builder.set_write_only(true, &test_config_path(), 5).unwrap();
        builder.set_numeric_ids(true, &test_config_path(), 6).unwrap();
        builder.set_listable(false, &test_config_path(), 7).unwrap();
        builder.set_uid(1000, &test_config_path(), 8).unwrap();
        builder.set_gid(100, &test_config_path(), 9).unwrap();
        builder.set_timeout(NonZeroU64::new(300), &test_config_path(), 10).unwrap();
        builder.set_max_connections(NonZeroU32::new(5), &test_config_path(), 11).unwrap();
        builder.set_bandwidth_limit(
            NonZeroU64::new(1000),
            NonZeroU64::new(2000),
            true,
            &test_config_path(),
            12,
        ).unwrap();

        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();

        assert_eq!(def.name, "fullmod");
        assert_eq!(def.path, PathBuf::from("/full/path"));
        assert_eq!(def.comment.as_deref(), Some("Full test"));
        assert!(!def.read_only);
        assert!(def.write_only);
        assert!(def.numeric_ids);
        assert!(!def.listable);
        assert_eq!(def.uid, Some(1000));
        assert_eq!(def.gid, Some(100));
        assert_eq!(def.timeout, NonZeroU64::new(300));
        assert_eq!(def.max_connections, NonZeroU32::new(5));
        assert_eq!(def.bandwidth_limit, NonZeroU64::new(1000));
        assert_eq!(def.bandwidth_burst, NonZeroU64::new(2000));
        assert!(def.bandwidth_burst_specified);
        assert!(def.bandwidth_limit_specified);
        assert!(def.bandwidth_limit_configured);
    }

    #[test]
    fn finish_uses_default_values_for_unset_fields() {
        let mut builder = ModuleDefinitionBuilder::new("defaults".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();

        let result = builder.finish(&test_config_path(), None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();

        // Verify defaults
        assert!(def.read_only); // default true
        assert!(!def.write_only); // default false
        assert!(!def.numeric_ids); // default false
        assert!(def.listable); // default true
        assert!(def.use_chroot); // default true
        assert!(def.hosts_allow.is_empty());
        assert!(def.hosts_deny.is_empty());
        assert!(def.auth_users.is_empty());
        assert!(def.refuse_options.is_empty());
        assert!(def.uid.is_none());
        assert!(def.gid.is_none());
        assert!(def.timeout.is_none());
        assert!(def.max_connections.is_none());
        assert!(def.bandwidth_limit.is_none());
        assert!(!def.bandwidth_limit_specified);
        assert!(!def.bandwidth_limit_configured);
    }
}
