//! INI-style parser for `rsyncd.conf` files.
//!
//! Handles line-by-line parsing of global directives, `[module]` section
//! headers, and per-module key-value pairs. Upstream: `loadparm.c`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::RsyncdConfig;
use super::sections::{GlobalConfig, ModuleConfig};
use super::validation::ConfigError;

pub(crate) struct Parser<'a> {
    input: &'a str,
    path: &'a Path,
    line_number: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a str, path: &'a Path) -> Self {
        Self {
            input,
            path,
            line_number: 0,
        }
    }

    pub(crate) fn parse(&mut self) -> Result<RsyncdConfig, ConfigError> {
        let mut global = GlobalConfig::default();
        let mut modules = Vec::new();
        let mut current_module: Option<ModuleBuilder> = None;
        let mut module_names = HashMap::new();

        for line in self.input.lines() {
            self.line_number += 1;
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }

            if trimmed.starts_with('[') {
                if let Some(builder) = current_module.take() {
                    let module = builder.build(self.path)?;
                    modules.push(module);
                }

                let end = trimmed.find(']').ok_or_else(|| {
                    ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "unterminated module header",
                    )
                })?;

                let name = trimmed[1..end].trim();
                if name.is_empty() {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "module name must be non-empty",
                    ));
                }

                if let Some(prev_line) = module_names.get(name) {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        format!(
                            "duplicate module '{name}' (previously defined at line {prev_line})"
                        ),
                    ));
                }
                module_names.insert(name.to_string(), self.line_number);

                let trailing = trimmed[end + 1..].trim();
                if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';')
                {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "unexpected content after module header",
                    ));
                }

                current_module = Some(ModuleBuilder::new(name.to_string(), self.line_number));
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                ConfigError::parse_error(
                    self.path,
                    self.line_number,
                    "expected 'key = value' format",
                )
            })?;

            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            if let Some(ref mut builder) = current_module {
                self.parse_module_directive(builder, &key, value)?;
            } else {
                self.parse_global_directive(&mut global, &key, value)?;
            }
        }

        if let Some(builder) = current_module {
            let module = builder.build(self.path)?;
            modules.push(module);
        }

        #[cfg(feature = "daemon-tls")]
        self.validate_tls_config(&global)?;

        Ok(RsyncdConfig { global, modules })
    }

    fn parse_global_directive(
        &self,
        global: &mut GlobalConfig,
        key: &str,
        value: &str,
    ) -> Result<(), ConfigError> {
        match key {
            "port" => {
                global.port = value.parse().map_err(|_| {
                    ConfigError::parse_error(self.path, self.line_number, "invalid port number")
                })?;
            }
            "address" => {
                global.address = Some(value.to_string());
            }
            "motd file" => {
                global.motd_file = Some(PathBuf::from(value));
            }
            "log file" => {
                global.log_file = Some(PathBuf::from(value));
            }
            "pid file" => {
                global.pid_file = Some(PathBuf::from(value));
            }
            "socket options" => {
                global.socket_options = Some(value.to_string());
            }
            "log format" => {
                global.log_format = Some(value.to_string());
            }
            "syslog facility" => {
                global.syslog_facility = Some(value.to_string());
            }
            "syslog tag" => {
                global.syslog_tag = Some(value.to_string());
            }
            "uid" => {
                if value.is_empty() {
                    return Err(ConfigError::validation_error(
                        self.path,
                        self.line_number,
                        "uid must not be empty",
                    ));
                }
                global.uid = Some(value.to_string());
            }
            "gid" => {
                if value.is_empty() {
                    return Err(ConfigError::validation_error(
                        self.path,
                        self.line_number,
                        "gid must not be empty",
                    ));
                }
                global.gid = Some(value.to_string());
            }
            "listen backlog" => {
                global.listen_backlog = Some(value.parse().map_err(|_| {
                    ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "invalid listen backlog value",
                    )
                })?);
            }
            "proxy protocol" => {
                global.proxy_protocol = match value.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    _ => {
                        return Err(ConfigError::parse_error(
                            self.path,
                            self.line_number,
                            format!("invalid boolean value '{value}' for 'proxy protocol'"),
                        ));
                    }
                };
            }
            "daemon chroot" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "'daemon chroot' must not be empty",
                    ));
                }
                global.daemon_chroot = Some(PathBuf::from(trimmed));
            }
            #[cfg(feature = "daemon-tls")]
            "ssl cert" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "'ssl cert' must not be empty",
                    ));
                }
                global.ssl_cert = Some(PathBuf::from(trimmed));
            }
            #[cfg(feature = "daemon-tls")]
            "ssl key" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "'ssl key' must not be empty",
                    ));
                }
                global.ssl_key = Some(PathBuf::from(trimmed));
            }
            #[cfg(feature = "daemon-tls")]
            "ssl ca" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "'ssl ca' must not be empty",
                    ));
                }
                global.ssl_ca = Some(PathBuf::from(trimmed));
            }
            _ => {
                eprintln!(
                    "warning: unknown global directive '{}' at line {} in '{}'",
                    key,
                    self.line_number,
                    self.path.display(),
                );
            }
        }
        Ok(())
    }

    fn parse_module_directive(
        &self,
        builder: &mut ModuleBuilder,
        key: &str,
        value: &str,
    ) -> Result<(), ConfigError> {
        match key {
            "path" => {
                if value.is_empty() {
                    return Err(ConfigError::validation_error(
                        self.path,
                        self.line_number,
                        "path must not be empty",
                    ));
                }
                builder.path = Some(PathBuf::from(value));
            }
            "comment" => {
                builder.comment = Some(value.to_string());
            }
            "read only" => {
                builder.read_only = Some(self.parse_bool(value)?);
            }
            "write only" => {
                builder.write_only = Some(self.parse_bool(value)?);
            }
            "list" => {
                builder.list = Some(self.parse_bool(value)?);
            }
            "uid" => {
                builder.uid = Some(value.to_string());
            }
            "gid" => {
                builder.gid = Some(value.to_string());
            }
            "max connections" => {
                builder.max_connections = Some(value.parse().map_err(|_| {
                    ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "invalid max connections value",
                    )
                })?);
            }
            "lock file" => {
                builder.lock_file = Some(PathBuf::from(value));
            }
            "auth users" => {
                builder.auth_users = self.parse_list(value);
            }
            "secrets file" => {
                if value.is_empty() {
                    return Err(ConfigError::validation_error(
                        self.path,
                        self.line_number,
                        "secrets file must not be empty",
                    ));
                }
                builder.secrets_file = Some(PathBuf::from(value));
            }
            "hosts allow" => {
                builder.hosts_allow = self.parse_list(value);
            }
            "hosts deny" => {
                builder.hosts_deny = self.parse_list(value);
            }
            "exclude" => {
                builder.exclude.push(value.to_string());
            }
            "include" => {
                builder.include.push(value.to_string());
            }
            "filter" => {
                builder.filter.push(value.to_string());
            }
            "exclude from" => {
                builder.exclude_from = Some(PathBuf::from(value));
            }
            "include from" => {
                builder.include_from = Some(PathBuf::from(value));
            }
            "incoming chmod" => {
                builder.incoming_chmod = Some(value.to_string());
            }
            "outgoing chmod" => {
                builder.outgoing_chmod = Some(value.to_string());
            }
            "timeout" => {
                builder.timeout = Some(value.parse().map_err(|_| {
                    ConfigError::parse_error(self.path, self.line_number, "invalid timeout value")
                })?);
            }
            "max verbosity" => {
                builder.max_verbosity = Some(value.parse().map_err(|_| {
                    ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "invalid max verbosity value",
                    )
                })?);
            }
            "use chroot" => {
                builder.use_chroot = Some(self.parse_bool(value)?);
            }
            "numeric ids" => {
                builder.numeric_ids = Some(self.parse_bool(value)?);
            }
            "fake super" => {
                builder.fake_super = Some(self.parse_bool(value)?);
            }
            "transfer logging" => {
                builder.transfer_logging = Some(self.parse_bool(value)?);
            }
            "refuse options" => {
                builder.refuse_options = self.parse_list(value);
            }
            "dont compress" => {
                builder.dont_compress = self.parse_list(value);
            }
            "early exec" => {
                builder.early_exec = Some(value.to_string());
            }
            "pre-xfer exec" => {
                builder.pre_xfer_exec = Some(value.to_string());
            }
            "post-xfer exec" => {
                builder.post_xfer_exec = Some(value.to_string());
            }
            "name converter" => {
                builder.name_converter = Some(value.to_string());
            }
            "strict modes" => {
                builder.strict_modes = Some(self.parse_bool(value)?);
            }
            "open noatime" => {
                builder.open_noatime = Some(self.parse_bool(value)?);
            }
            "charset" => {
                builder.charset = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "temp dir" => {
                builder.temp_dir = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                };
            }
            "forward lookup" => {
                builder.forward_lookup = Some(self.parse_bool(value)?);
            }
            "reverse lookup" => {
                builder.reverse_lookup = Some(self.parse_bool(value)?);
            }
            "ignore errors" => {
                builder.ignore_errors = Some(self.parse_bool(value)?);
            }
            "ignore nonreadable" => {
                builder.ignore_nonreadable = Some(self.parse_bool(value)?);
            }
            "munge symlinks" => {
                builder.munge_symlinks = Some(Some(self.parse_bool(value)?));
            }
            // upstream: daemon-parm.txt - these are P_LOCAL directives that can
            // appear at module level to override the global defaults.
            "log file" => {
                builder.module_log_file = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                };
            }
            "log format" => {
                builder.module_log_format = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "syslog facility" => {
                builder.module_syslog_facility = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "syslog tag" => {
                builder.module_syslog_tag = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            _ => {
                eprintln!(
                    "warning: unknown per-module directive '{}' at line {} in '{}'",
                    key,
                    self.line_number,
                    self.path.display(),
                );
            }
        }
        Ok(())
    }

    fn parse_bool(&self, value: &str) -> Result<bool, ConfigError> {
        match value.to_ascii_lowercase().as_str() {
            "yes" | "true" | "1" => Ok(true),
            "no" | "false" | "0" => Ok(false),
            _ => Err(ConfigError::parse_error(
                self.path,
                self.line_number,
                format!("invalid boolean value '{value}'"),
            )),
        }
    }

    fn parse_list(&self, value: &str) -> Vec<String> {
        value
            .split([',', ' '])
            .filter_map(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect()
    }

    /// Validates that `ssl cert` and `ssl key` are either both set or both absent.
    ///
    /// `ssl ca` is independently optional. Setting `ssl ca` without the cert/key
    /// pair is also an error - a CA bundle is meaningless without a server identity.
    #[cfg(feature = "daemon-tls")]
    fn validate_tls_config(&self, global: &GlobalConfig) -> Result<(), ConfigError> {
        let has_cert = global.ssl_cert.is_some();
        let has_key = global.ssl_key.is_some();
        let has_ca = global.ssl_ca.is_some();

        if has_cert && !has_key {
            return Err(ConfigError::cross_field_error(
                self.path,
                "'ssl cert' requires 'ssl key' to also be set",
            ));
        }
        if has_key && !has_cert {
            return Err(ConfigError::cross_field_error(
                self.path,
                "'ssl key' requires 'ssl cert' to also be set",
            ));
        }
        if has_ca && !has_cert {
            return Err(ConfigError::cross_field_error(
                self.path,
                "'ssl ca' requires 'ssl cert' and 'ssl key' to also be set",
            ));
        }
        Ok(())
    }
}

/// Builder for assembling a `ModuleConfig` from parsed key-value pairs.
///
/// Tracks which directives have been set and applies defaults for unspecified
/// values during `build()`. Upstream defaults mirror `loadparm.c`.
#[derive(Default)]
struct ModuleBuilder {
    name: String,
    line: usize,
    path: Option<PathBuf>,
    comment: Option<String>,
    read_only: Option<bool>,
    write_only: Option<bool>,
    list: Option<bool>,
    uid: Option<String>,
    gid: Option<String>,
    max_connections: Option<u32>,
    lock_file: Option<PathBuf>,
    auth_users: Vec<String>,
    secrets_file: Option<PathBuf>,
    hosts_allow: Vec<String>,
    hosts_deny: Vec<String>,
    exclude: Vec<String>,
    include: Vec<String>,
    filter: Vec<String>,
    exclude_from: Option<PathBuf>,
    include_from: Option<PathBuf>,
    incoming_chmod: Option<String>,
    outgoing_chmod: Option<String>,
    timeout: Option<u32>,
    max_verbosity: Option<i32>,
    use_chroot: Option<bool>,
    numeric_ids: Option<bool>,
    fake_super: Option<bool>,
    transfer_logging: Option<bool>,
    refuse_options: Vec<String>,
    dont_compress: Vec<String>,
    early_exec: Option<String>,
    pre_xfer_exec: Option<String>,
    post_xfer_exec: Option<String>,
    name_converter: Option<String>,
    strict_modes: Option<bool>,
    open_noatime: Option<bool>,
    charset: Option<String>,
    temp_dir: Option<PathBuf>,
    forward_lookup: Option<bool>,
    reverse_lookup: Option<bool>,
    ignore_errors: Option<bool>,
    ignore_nonreadable: Option<bool>,
    munge_symlinks: Option<Option<bool>>,
    module_log_file: Option<PathBuf>,
    module_log_format: Option<String>,
    module_syslog_facility: Option<String>,
    module_syslog_tag: Option<String>,
}

impl ModuleBuilder {
    fn new(name: String, line: usize) -> Self {
        Self {
            name,
            line,
            ..Default::default()
        }
    }

    fn build(self, path: &Path) -> Result<ModuleConfig, ConfigError> {
        let path_buf = self.path.ok_or_else(|| {
            ConfigError::validation_error(
                path,
                self.line,
                format!(
                    "module '{}' is missing required 'path' directive",
                    self.name
                ),
            )
        })?;

        Ok(ModuleConfig {
            name: self.name,
            path: path_buf,
            comment: self.comment,
            read_only: self.read_only.unwrap_or(true),
            write_only: self.write_only.unwrap_or(false),
            list: self.list.unwrap_or(true),
            uid: self.uid,
            gid: self.gid,
            max_connections: self.max_connections.unwrap_or(0),
            lock_file: self.lock_file,
            auth_users: self.auth_users,
            secrets_file: self.secrets_file,
            hosts_allow: self.hosts_allow,
            hosts_deny: self.hosts_deny,
            exclude: self.exclude,
            include: self.include,
            filter: self.filter,
            exclude_from: self.exclude_from,
            include_from: self.include_from,
            incoming_chmod: self.incoming_chmod,
            outgoing_chmod: self.outgoing_chmod,
            timeout: self.timeout,
            max_verbosity: self.max_verbosity.unwrap_or(1),
            use_chroot: self.use_chroot.unwrap_or(true),
            numeric_ids: self.numeric_ids.unwrap_or(false),
            fake_super: self.fake_super.unwrap_or(false),
            transfer_logging: self.transfer_logging.unwrap_or(false),
            refuse_options: self.refuse_options,
            dont_compress: self.dont_compress,
            early_exec: self.early_exec,
            pre_xfer_exec: self.pre_xfer_exec,
            post_xfer_exec: self.post_xfer_exec,
            name_converter: self.name_converter,
            strict_modes: self.strict_modes.unwrap_or(true),
            open_noatime: self.open_noatime.unwrap_or(false),
            charset: self.charset,
            temp_dir: self.temp_dir,
            forward_lookup: self.forward_lookup.unwrap_or(true),
            reverse_lookup: self.reverse_lookup.unwrap_or(true),
            ignore_errors: self.ignore_errors.unwrap_or(false),
            ignore_nonreadable: self.ignore_nonreadable.unwrap_or(false),
            munge_symlinks: self.munge_symlinks.unwrap_or(None),
            module_log_file: self.module_log_file,
            module_log_format: self.module_log_format,
            module_syslog_facility: self.module_syslog_facility,
            module_syslog_tag: self.module_syslog_tag,
        })
    }
}
