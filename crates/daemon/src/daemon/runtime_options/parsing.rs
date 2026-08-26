impl RuntimeOptions {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn parse(arguments: &[OsString]) -> Result<Self, DaemonError> {
        Self::parse_with_brand(arguments, Brand::Oc, true)
    }

    /// Reports whether this parser recognises the option spelling `option`.
    ///
    /// The answer comes from the parser itself rather than a second list, so it
    /// cannot drift from the chain below. A value-taking option named without
    /// its value still fails, but with a missing-argument error; only
    /// `unsupported_option` means the spelling is unknown. Host configuration is
    /// excluded so the answer depends on the parser alone.
    #[cfg(test)]
    pub(crate) fn option_is_recognised(option: &str) -> bool {
        let arguments = [OsString::from(option)];
        match Self::parse_with_brand(&arguments, Brand::Oc, false) {
            Ok(_) => true,
            Err(error) => !error.to_string().contains("unknown option"),
        }
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
                // upstream: clientserver.c:1573 - `--port 0` is treated as
                // "unspecified": it does not override a config `port` directive
                // and falls through to the 873 default below. Only a non-zero
                // CLI port suppresses the config value.
                options.port_overridden = options.port != 0;
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
                let rate = parse_runtime_bwlimit(&value)?;
                options.set_bandwidth_limit(rate)?;
            } else if argument == "--no-bwlimit" {
                options.set_bandwidth_limit(None)?;
            } else if argument == "--once" {
                options.set_max_sessions(NonZeroUsize::new(1).expect("1 is nonzero"))?;
            } else if argument == "--no-detach" {
                options.detach = false;
            } else if argument == "--detach" {
                options.detach = true;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--max-sessions")? {
                let max = parse_max_sessions(&value)?;
                options.set_max_sessions(max)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--max-connections")?
            {
                let max = parse_max_connections(&value)?;
                options.set_max_connections(max)?;
            } else if argument == "--ipv4" || argument == "-4" {
                // upstream: help-rsyncd.h lists `--ipv4, -4` / `--ipv6, -6`, so
                // both spellings must reach the same address-family decision.
                options.force_address_family(AddressFamily::Ipv4)?;
            } else if argument == "--ipv6" || argument == "-6" {
                options.force_address_family(AddressFamily::Ipv6)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--tcp-fastopen")? {
                options.set_tcp_fastopen(parse_tcp_fastopen_mode(&value, brand)?);
            } else if let Some(value) = take_option_value(argument, &mut iter, "--log-file")? {
                options.set_log_file(PathBuf::from(value))?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--lock-file")? {
                options.set_lock_file(PathBuf::from(value))?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--secrets-file")? {
                let validated = validate_cli_secrets_file(PathBuf::from(value))?;
                options.set_cli_secrets_file(validated)?;
            } else if let Some(value) = take_option_value(argument, &mut iter, "--pid-file")? {
                options.set_pid_file(PathBuf::from(value))?;
            } else if argument == "--verbose" {
                options.verbosity = options.verbosity.saturating_add(1);
            } else if argument == "--no-verbose" || argument == "--no-v" {
                options.verbosity = 0;
            } else if is_stacked_short_verbose(argument) {
                let extra = argument.to_string_lossy().matches('v').count();
                options.verbosity = options.verbosity.saturating_add(extra as u8);
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

        // upstream: clientserver.c:1573-1574 -
        //   `if (rsync_port == 0 && (rsync_port = lp_rsync_port()) == 0)
        //        rsync_port = RSYNC_PORT;`
        // After CLI and config resolution, a still-zero port coerces to the
        // well-known rsync port 873 rather than binding a kernel-assigned
        // ephemeral port. Applied once here so both the sync and async daemon
        // bind paths share the same 0 -> 873 coercion.
        if options.port == 0 {
            options.port = DEFAULT_PORT;
        }

        // QUIC listener identity is a certificate/key pair: the listener needs
        // both to present an identity, and upstream rsync-ssl keeps
        // RSYNC_SSL_CERT / RSYNC_SSL_KEY distinct (docs/design/quic-transport-
        // policy.md, decision A). Enforce both-or-neither once, after CLI, env,
        // and every config file have merged, so a key split across sources still
        // validates. Fail loudly rather than silently binding an unexpected
        // self-signed identity when only half the pair is named.
        #[cfg(feature = "quic")]
        match (
            options.quic_cert_file.is_some(),
            options.quic_key_file.is_some(),
        ) {
            (true, false) => {
                return Err(config_error(
                    "'quic cert file' requires 'quic key file' to be set as well".to_owned(),
                ));
            }
            (false, true) => {
                return Err(config_error(
                    "'quic key file' requires 'quic cert file' to be set as well".to_owned(),
                ));
            }
            _ => {}
        }

        Ok(options)
    }
}

/// Returns `true` when `arg` is `-v`, `-vv`, `-vvv`, ... (a hyphen followed
/// by one or more `v` characters and nothing else).
///
/// upstream: options.c stacks short flags so `-vvv` increments `verbose` by
/// three. The daemon accepts the same stacked form via popt's short-option
/// dispatch.
fn is_stacked_short_verbose(arg: &OsString) -> bool {
    let bytes = arg.as_encoded_bytes();
    bytes.len() >= 2 && bytes[0] == b'-' && bytes[1..].iter().all(|&b| b == b'v')
}
