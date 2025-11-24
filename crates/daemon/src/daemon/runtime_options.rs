#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeOptions {
    brand: Brand,
    bind_address: IpAddr,
    port: u16,
    max_sessions: Option<NonZeroUsize>,
    modules: Vec<ModuleDefinition>,
    motd_lines: Vec<String>,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_limit_configured: bool,
    address_family: Option<AddressFamily>,
    bind_address_overridden: bool,
    log_file: Option<PathBuf>,
    log_file_configured: bool,
    global_refuse_options: Option<Vec<String>>,
    global_secrets_file: Option<PathBuf>,
    global_secrets_from_config: bool,
    pid_file: Option<PathBuf>,
    pid_file_from_config: bool,
    reverse_lookup: bool,
    reverse_lookup_configured: bool,
    lock_file: Option<PathBuf>,
    lock_file_from_config: bool,
    delegate_arguments: Vec<OsString>,
    inline_modules: bool,
    global_incoming_chmod: Option<String>,
    global_outgoing_chmod: Option<String>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            brand: Brand::Oc,
            bind_address: DEFAULT_BIND_ADDRESS,
            port: DEFAULT_PORT,
            max_sessions: None,
            modules: Vec::new(),
            motd_lines: Vec::new(),
            bandwidth_limit: None,
            bandwidth_burst: None,
            bandwidth_limit_configured: false,
            address_family: None,
            bind_address_overridden: false,
            log_file: None,
            log_file_configured: false,
            global_refuse_options: None,
            global_secrets_file: None,
            global_secrets_from_config: false,
            pid_file: None,
            pid_file_from_config: false,
            reverse_lookup: true,
            reverse_lookup_configured: false,
            lock_file: None,
            lock_file_from_config: false,
            delegate_arguments: Vec::new(),
            inline_modules: false,
            global_incoming_chmod: None,
            global_outgoing_chmod: None,
        }
    }
}

impl RuntimeOptions {
    #[cfg(test)]
    #[allow(dead_code)]
    fn parse(arguments: &[OsString]) -> Result<Self, DaemonError> {
        Self::parse_with_brand(arguments, Brand::Oc, true)
    }

    fn parse_with_brand(
        arguments: &[OsString],
        brand: Brand,
        load_defaults: bool,
    ) -> Result<Self, DaemonError> {
        let mut options = Self::default();
        options.brand = brand;
        let mut seen_modules = HashSet::new();
        if load_defaults && !config_argument_present(arguments) {
            if let Some(path) = environment_config_override() {
                options.delegate_arguments.push(OsString::from("--config"));
                options.delegate_arguments.push(path.clone());
                options.load_config_modules(&path, &mut seen_modules)?;
            } else if let Some(path) = default_config_path_if_present(brand) {
                options.delegate_arguments.push(OsString::from("--config"));
                options.delegate_arguments.push(path.clone());
                options.load_config_modules(&path, &mut seen_modules)?;
            }
        }

        if load_defaults && options.global_secrets_file.is_none() {
            if let Some((path, env)) = environment_secrets_override() {
                let path_buf = PathBuf::from(&path);
                if let Some(validated) = validate_secrets_file_from_env(&path_buf, env)? {
                    options.global_secrets_file = Some(validated.clone());
                    options.global_secrets_from_config = false;
                    options
                        .delegate_arguments
                        .push(OsString::from("--secrets-file"));
                    options.delegate_arguments.push(validated.into_os_string());
                }
            } else if let Some(path) = default_secrets_path_if_present(brand) {
                options.global_secrets_file = Some(PathBuf::from(&path));
                options.global_secrets_from_config = false;
                options
                    .delegate_arguments
                    .push(OsString::from("--secrets-file"));
                options.delegate_arguments.push(path);
            }
        }

        let mut iter = arguments.iter();

        while let Some(argument) = iter.next() {
            if let Some(value) = take_option_value(argument, &mut iter, "--port")? {
                options.port = parse_port(&value)?;
                options.delegate_arguments.push(OsString::from("--port"));
                options.delegate_arguments.push(value.clone());
            } else if let Some(value) = take_option_value(argument, &mut iter, "--bind")? {
                let addr = parse_bind_address(&value)?;
                options.set_bind_address(addr)?;
                options.delegate_arguments.push(OsString::from("--address"));
                options.delegate_arguments.push(value.clone());
            } else if let Some(value) = take_option_value(argument, &mut iter, "--address")? {
                let addr = parse_bind_address(&value)?;
                options.set_bind_address(addr)?;
                options.delegate_arguments.push(OsString::from("--address"));
                options.delegate_arguments.push(value.clone());
            } else if let Some(value) = take_option_value(argument, &mut iter, "--config")? {
                options.delegate_arguments.push(OsString::from("--config"));
                options.delegate_arguments.push(value.clone());
                options.load_config_modules(&value, &mut seen_modules)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd-file")? {
                options.load_motd_file(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd")? {
                options.load_motd_file(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--motd-line")? {
                options.push_motd_line(value);
            } else if let Some(value) = take_option_value(argument, &mut iter, "--bwlimit")? {
                let components = parse_runtime_bwlimit(&value)?;
                options.set_bandwidth_limit(components.rate(), components.burst())?;
                options.delegate_arguments.push(OsString::from("--bwlimit"));
                options.delegate_arguments.push(value.clone());
            } else if argument == "--no-bwlimit" {
                options.set_bandwidth_limit(None, None)?;
                options.delegate_arguments.push(OsString::from("--bwlimit"));
                options.delegate_arguments.push(OsString::from("0"));
            } else if argument == "--once" {
                options.set_max_sessions(NonZeroUsize::new(1).unwrap())?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--max-sessions")? {
                let max = parse_max_sessions(&value)?;
                options.set_max_sessions(max)?;
            } else if argument == "--ipv4" {
                options.force_address_family(AddressFamily::Ipv4)?;
                options.delegate_arguments.push(OsString::from("--ipv4"));
            } else if argument == "--ipv6" {
                options.force_address_family(AddressFamily::Ipv6)?;
                options.delegate_arguments.push(OsString::from("--ipv6"));
            } else if let Some(value) = take_option_value(argument, &mut iter, "--log-file")? {
                options.set_log_file(PathBuf::from(value.clone()))?;
                options
                    .delegate_arguments
                    .push(OsString::from("--log-file"));
                options.delegate_arguments.push(value.clone());
            } else if let Some(value) = take_option_value(argument, &mut iter, "--lock-file")? {
                options.set_lock_file(PathBuf::from(value.clone()))?;
                options
                    .delegate_arguments
                    .push(OsString::from("--lock-file"));
                options.delegate_arguments.push(value.clone());
            } else if let Some(value) = take_option_value(argument, &mut iter, "--pid-file")? {
                options.set_pid_file(PathBuf::from(value.clone()))?;
                options
                    .delegate_arguments
                    .push(OsString::from("--pid-file"));
                options.delegate_arguments.push(value.clone());
            } else if argument == "--module" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value("--module"))?;
                let mut module =
                    parse_module_definition(
                        value,
                        options.global_secrets_file.as_deref(),
                        options.global_incoming_chmod.as_deref(),
                        options.global_outgoing_chmod.as_deref(),
                    )?;
                if let Some(global) = &options.global_refuse_options {
                    module.inherit_refuse_options(global);
                }
                if !seen_modules.insert(module.name.clone()) {
                    return Err(duplicate_module(&module.name));
                }
                options.modules.push(module);
                options.inline_modules = true;
            } else {
                return Err(unsupported_option(argument.clone(), brand));
            }
        }

        Ok(options)
    }

    fn set_max_sessions(&mut self, value: NonZeroUsize) -> Result<(), DaemonError> {
        if self.max_sessions.is_some() {
            return Err(duplicate_argument("--max-sessions"));
        }

        self.max_sessions = Some(value);
        Ok(())
    }

    fn set_bandwidth_limit(
        &mut self,
        limit: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
    ) -> Result<(), DaemonError> {
        if self.bandwidth_limit_configured {
            return Err(duplicate_argument("--bwlimit"));
        }

        self.bandwidth_limit = limit;
        self.bandwidth_burst = burst;
        self.bandwidth_limit_configured = true;
        Ok(())
    }

    fn set_log_file(&mut self, path: PathBuf) -> Result<(), DaemonError> {
        if self.log_file_configured {
            return Err(duplicate_argument("--log-file"));
        }

        self.log_file = Some(path);
        self.log_file_configured = true;
        Ok(())
    }

    fn set_lock_file(&mut self, path: PathBuf) -> Result<(), DaemonError> {
        if let Some(existing) = &self.lock_file {
            if !self.lock_file_from_config {
                return Err(duplicate_argument("--lock-file"));
            }

            if existing == &path {
                self.lock_file_from_config = false;
                return Ok(());
            }
        }

        self.lock_file = Some(path);
        self.lock_file_from_config = false;
        Ok(())
    }

    fn set_pid_file(&mut self, path: PathBuf) -> Result<(), DaemonError> {
        if let Some(existing) = &self.pid_file {
            if !self.pid_file_from_config {
                return Err(duplicate_argument("--pid-file"));
            }

            if existing == &path {
                self.pid_file_from_config = false;
                return Ok(());
            }
        }

        self.pid_file = Some(path);
        self.pid_file_from_config = false;
        Ok(())
    }

    fn set_bind_address(&mut self, addr: IpAddr) -> Result<(), DaemonError> {
        if let Some(family) = self.address_family {
            if !family.matches(addr) {
                return Err(match family {
                    AddressFamily::Ipv4 => config_error(
                        "cannot bind an IPv6 address when --ipv4 is specified".to_string(),
                    ),
                    AddressFamily::Ipv6 => config_error(
                        "cannot bind an IPv4 address when --ipv6 is specified".to_string(),
                    ),
                });
            }
        } else {
            self.address_family = Some(AddressFamily::from_ip(addr));
        }

        self.bind_address = addr;
        self.bind_address_overridden = true;
        Ok(())
    }

    fn force_address_family(&mut self, family: AddressFamily) -> Result<(), DaemonError> {
        if let Some(existing) = self.address_family {
            if existing != family {
                let text = if self.bind_address_overridden {
                    match existing {
                        AddressFamily::Ipv4 => {
                            "cannot use --ipv6 with an IPv4 bind address".to_string()
                        }
                        AddressFamily::Ipv6 => {
                            "cannot use --ipv4 with an IPv6 bind address".to_string()
                        }
                    }
                } else {
                    "cannot combine --ipv4 with --ipv6".to_string()
                };
                return Err(config_error(text));
            }
        } else {
            self.address_family = Some(family);
        }

        match family {
            AddressFamily::Ipv4 => {
                if matches!(self.bind_address, IpAddr::V6(_)) {
                    if self.bind_address_overridden {
                        return Err(config_error(
                            "cannot use --ipv4 with an IPv6 bind address".to_string(),
                        ));
                    }
                    self.bind_address = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
                } else if !self.bind_address_overridden {
                    self.bind_address = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
                }
            }
            AddressFamily::Ipv6 => {
                if matches!(self.bind_address, IpAddr::V4(_)) {
                    if self.bind_address_overridden {
                        return Err(config_error(
                            "cannot use --ipv6 with an IPv4 bind address".to_string(),
                        ));
                    }
                    self.bind_address = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
                } else if !self.bind_address_overridden {
                    self.bind_address = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
                }
            }
        }

        Ok(())
    }

    fn load_config_modules(
        &mut self,
        value: &OsString,
        seen_modules: &mut HashSet<String>,
    ) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let parsed = parse_config_modules(&path)?;

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

        if let Some((components, _origin)) = parsed.global_bandwidth_limit {
            if !self.bandwidth_limit_configured {
                self.bandwidth_limit = components.rate();
                self.bandwidth_burst = components.burst();
                self.bandwidth_limit_configured = true;
            }
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
        if let Some(existing) = &self.global_secrets_file {
            if self.global_secrets_from_config {
                if existing == &path {
                    return Ok(());
                }

                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    "duplicate 'secrets file' directive in global section",
                ));
            }
        }

        self.global_secrets_file = Some(path);
        self.global_secrets_from_config = true;
        Ok(())
    }

    fn load_motd_file(&mut self, value: &OsString) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let contents =
            fs::read_to_string(&path).map_err(|error| config_io_error("read", &path, error))?;

        for raw_line in contents.lines() {
            let line = raw_line.trim_end_matches('\r').to_string();
            self.motd_lines.push(line);
        }

        Ok(())
    }

    fn push_motd_line(&mut self, value: OsString) {
        let line = value
            .to_string_lossy()
            .trim_matches(['\r', '\n'])
            .to_string();
        self.motd_lines.push(line);
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl RuntimeOptions {
    pub(super) fn modules(&self) -> &[ModuleDefinition] {
        &self.modules
    }

    pub(super) fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    pub(super) fn bandwidth_burst(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    pub(super) fn brand(&self) -> Brand {
        self.brand
    }

    pub(super) fn bandwidth_limit_configured(&self) -> bool {
        self.bandwidth_limit_configured
    }

    pub(super) fn bind_address(&self) -> IpAddr {
        self.bind_address
    }

    pub(super) fn address_family(&self) -> Option<AddressFamily> {
        self.address_family
    }

    pub(super) fn motd_lines(&self) -> &[String] {
        &self.motd_lines
    }

    pub(super) fn log_file(&self) -> Option<&PathBuf> {
        self.log_file.as_ref()
    }

    pub(super) fn pid_file(&self) -> Option<&Path> {
        self.pid_file.as_deref()
    }

    pub(super) fn reverse_lookup(&self) -> bool {
        self.reverse_lookup
    }

    pub(super) fn lock_file(&self) -> Option<&Path> {
        self.lock_file.as_deref()
    }
}

