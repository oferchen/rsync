impl RuntimeOptions {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn parse(arguments: &[OsString]) -> Result<Self, DaemonError> {
        Self::parse_with_brand(arguments, Brand::Oc, true)
    }

    fn parse_with_brand(
        arguments: &[OsString],
        brand: Brand,
        load_defaults: bool,
    ) -> Result<Self, DaemonError> {
        let mut options = Self {
            brand,
            ..Default::default()
        };
        let mut seen_modules = HashSet::new();
        if load_defaults && !config_argument_present(arguments) {
            if let Some(path) = environment_config_override() {
                options.load_config_modules(&path, &mut seen_modules)?;
            } else if let Some(path) = default_config_path_if_present(brand) {
                options.load_config_modules(&path, &mut seen_modules)?;
            }
        }

        if load_defaults && options.global_secrets_file.is_none() {
            if let Some((path, env)) = environment_secrets_override() {
                let path_buf = PathBuf::from(&path);
                if let Some(validated) = validate_secrets_file_from_env(&path_buf, env)? {
                    options.global_secrets_file = Some(validated);
                    options.global_secrets_from_config = false;
                }
            } else if let Some(path) = default_secrets_path_if_present(brand) {
                options.global_secrets_file = Some(PathBuf::from(&path));
                options.global_secrets_from_config = false;
            }
        }

        let mut iter = arguments.iter();

        while let Some(argument) = iter.next() {
            if let Some(value) = take_option_value(argument, &mut iter, "--port")? {
                options.port = parse_port(&value)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--bind")? {
                let addr = parse_bind_address(&value)?;
                options.set_bind_address(addr)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--address")? {
                let addr = parse_bind_address(&value)?;
                options.set_bind_address(addr)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--config")? {
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
            } else if argument == "--no-bwlimit" {
                options.set_bandwidth_limit(None, None)?;
            } else if argument == "--once" {
                options.set_max_sessions(NonZeroUsize::new(1).unwrap())?;
            } else if argument == "--no-detach" {
                options.detach = false;
            } else if argument == "--detach" {
                options.detach = true;
            } else if let Some(value) =
                take_option_value(argument, &mut iter, "--max-sessions")?
            {
                let max = parse_max_sessions(&value)?;
                options.set_max_sessions(max)?;
            } else if argument == "--ipv4" {
                options.force_address_family(AddressFamily::Ipv4)?;
            } else if argument == "--ipv6" {
                options.force_address_family(AddressFamily::Ipv6)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--log-file")? {
                options.set_log_file(PathBuf::from(value))?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--lock-file")? {
                options.set_lock_file(PathBuf::from(value))?;
            } else if let Some(value) =
                take_option_value(argument, &mut iter, "--secrets-file")?
            {
                let validated = validate_cli_secrets_file(PathBuf::from(value))?;
                options.set_cli_secrets_file(validated)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--pid-file")? {
                options.set_pid_file(PathBuf::from(value))?;
            } else if argument == "--module" {
                let value = iter
                    .next()
                    .ok_or_else(|| missing_argument_value("--module"))?;
                let mut module = parse_module_definition(
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
            } else {
                return Err(unsupported_option(argument.clone(), brand));
            }
        }

        Ok(options)
    }
}
