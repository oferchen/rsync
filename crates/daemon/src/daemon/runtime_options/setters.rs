impl RuntimeOptions {
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
                        "cannot bind an IPv6 address when --ipv4 is specified".to_owned(),
                    ),
                    AddressFamily::Ipv6 => config_error(
                        "cannot bind an IPv4 address when --ipv6 is specified".to_owned(),
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
                            "cannot use --ipv6 with an IPv4 bind address".to_owned()
                        }
                        AddressFamily::Ipv6 => {
                            "cannot use --ipv4 with an IPv6 bind address".to_owned()
                        }
                    }
                } else {
                    "cannot combine --ipv4 with --ipv6".to_owned()
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
                            "cannot use --ipv4 with an IPv6 bind address".to_owned(),
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
                            "cannot use --ipv6 with an IPv4 bind address".to_owned(),
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

    fn set_cli_secrets_file(&mut self, path: PathBuf) -> Result<(), DaemonError> {
        if self.global_secrets_from_cli {
            return Err(duplicate_argument("--secrets-file"));
        }

        let path = Some(path);
        path.clone_into(&mut self.global_secrets_file);
        self.global_secrets_from_config = false;
        self.global_secrets_from_cli = true;

        if let Some(global) = path {
            for module in &mut self.modules {
                if module.secrets_file.is_none() {
                    module.secrets_file = Some(global.clone());
                }
            }
        }

        Ok(())
    }

    fn load_motd_file(&mut self, value: &OsString) -> Result<(), DaemonError> {
        let path = PathBuf::from(value.clone());
        let contents =
            fs::read_to_string(&path).map_err(|error| config_io_error("read", &path, error))?;

        for raw_line in contents.lines() {
            let mut line = String::new();
            raw_line.trim_end_matches('\r').clone_into(&mut line);
            self.motd_lines.push(line);
        }

        Ok(())
    }

    fn push_motd_line(&mut self, value: OsString) {
        let mut line = String::new();
        value
            .to_string_lossy()
            .trim_matches(['\r', '\n'])
            .clone_into(&mut line);
        self.motd_lines.push(line);
    }
}
