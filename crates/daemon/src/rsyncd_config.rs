//! Daemon configuration file parsing for rsyncd.conf.
//!
//! This module provides a standalone API for parsing rsync daemon configuration
//! files matching upstream rsync 3.4.1 format. The configuration consists of
//! global parameters followed by per-module sections.
//!
//! # Format
//!
//! ```ini
//! # Global parameters
//! port = 873
//! motd file = /etc/rsyncd.motd
//! log file = /var/log/rsyncd.log
//!
//! [module_name]
//! path = /data/module_name
//! comment = Public files
//! read only = yes
//! ```
//!
//! # Example
//!
//! ```no_run
//! use daemon::rsyncd_config::RsyncdConfig;
//! use std::path::Path;
//!
//! let config = RsyncdConfig::from_file(Path::new("/etc/rsyncd.conf"))?;
//!
//! // Access global settings
//! println!("Port: {}", config.global().port());
//!
//! // Find a module
//! if let Some(module) = config.get_module("mymodule") {
//!     println!("Module path: {}", module.path().display());
//! }
//! # Ok::<(), daemon::rsyncd_config::ConfigError>(())
//! ```

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Errors that can occur while parsing configuration files.
#[derive(Debug, Clone)]
pub struct ConfigError {
    #[allow(dead_code)]
    kind: ErrorKind,
    line: Option<usize>,
    message: String,
    path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum ErrorKind {
    Io,
    Parse,
    Validation,
}

impl ConfigError {
    fn io_error(path: &Path, source: io::Error) -> Self {
        Self {
            kind: ErrorKind::Io,
            line: None,
            message: format!("failed to read '{}': {}", path.display(), source),
            path: Some(path.to_path_buf()),
        }
    }

    fn parse_error(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Parse,
            line: Some(line),
            message: message.into(),
            path: Some(path.to_path_buf()),
        }
    }

    fn validation_error(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Validation,
            line: Some(line),
            message: message.into(),
            path: Some(path.to_path_buf()),
        }
    }

    /// Returns the line number where the error occurred, if available.
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// Returns the configuration file path where the error occurred.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(f, "{}: ", path.display())?;
        }
        if let Some(line) = self.line {
            write!(f, "line {}: ", line)?;
        }
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Global configuration parameters.
///
/// These parameters appear before any module sections in the configuration file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlobalConfig {
    port: u16,
    address: Option<String>,
    motd_file: Option<PathBuf>,
    log_file: Option<PathBuf>,
    pid_file: Option<PathBuf>,
    socket_options: Option<String>,
    log_format: Option<String>,
}

impl GlobalConfig {
    /// Returns the daemon port (default: 873).
    pub fn port(&self) -> u16 {
        if self.port == 0 {
            873
        } else {
            self.port
        }
    }

    /// Returns the bind address, if specified.
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// Returns the MOTD file path, if specified.
    pub fn motd_file(&self) -> Option<&Path> {
        self.motd_file.as_deref()
    }

    /// Returns the log file path, if specified.
    pub fn log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Returns the PID file path, if specified.
    pub fn pid_file(&self) -> Option<&Path> {
        self.pid_file.as_deref()
    }

    /// Returns socket options string, if specified.
    pub fn socket_options(&self) -> Option<&str> {
        self.socket_options.as_deref()
    }

    /// Returns the log format string, if specified.
    pub fn log_format(&self) -> Option<&str> {
        self.log_format.as_deref()
    }
}

/// Per-module configuration parameters.
///
/// Each module represents a directory tree that can be accessed by clients.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleConfig {
    name: String,
    path: PathBuf,
    comment: Option<String>,
    read_only: bool,
    write_only: bool,
    list: bool,
    uid: Option<String>,
    gid: Option<String>,
    max_connections: u32,
    lock_file: Option<PathBuf>,
    auth_users: Vec<String>,
    secrets_file: Option<PathBuf>,
    hosts_allow: Vec<String>,
    hosts_deny: Vec<String>,
    exclude: Vec<String>,
    include: Vec<String>,
    filter: Vec<String>,
    timeout: Option<u32>,
    use_chroot: bool,
    numeric_ids: bool,
    fake_super: bool,
    transfer_logging: bool,
    refuse_options: Vec<String>,
    dont_compress: Vec<String>,
    pre_xfer_exec: Option<String>,
    post_xfer_exec: Option<String>,
}

impl ModuleConfig {
    /// Returns the module name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the module path (required).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the module comment, if specified.
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    /// Returns whether the module is read-only (default: true).
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Returns whether the module is write-only (default: false).
    pub fn write_only(&self) -> bool {
        self.write_only
    }

    /// Returns whether the module is listable (default: true).
    pub fn list(&self) -> bool {
        self.list
    }

    /// Returns the UID to run as, if specified.
    pub fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    /// Returns the GID to run as, if specified.
    pub fn gid(&self) -> Option<&str> {
        self.gid.as_deref()
    }

    /// Returns the maximum number of connections (0 = unlimited).
    pub fn max_connections(&self) -> u32 {
        self.max_connections
    }

    /// Returns the lock file path for this module, if specified.
    pub fn lock_file(&self) -> Option<&Path> {
        self.lock_file.as_deref()
    }

    /// Returns the list of authorized users.
    pub fn auth_users(&self) -> &[String] {
        &self.auth_users
    }

    /// Returns the secrets file path, if specified.
    pub fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }

    /// Returns the list of allowed host patterns.
    pub fn hosts_allow(&self) -> &[String] {
        &self.hosts_allow
    }

    /// Returns the list of denied host patterns.
    pub fn hosts_deny(&self) -> &[String] {
        &self.hosts_deny
    }

    /// Returns the list of exclude patterns.
    pub fn exclude(&self) -> &[String] {
        &self.exclude
    }

    /// Returns the list of include patterns.
    pub fn include(&self) -> &[String] {
        &self.include
    }

    /// Returns the list of filter rules.
    pub fn filter(&self) -> &[String] {
        &self.filter
    }

    /// Returns the I/O timeout in seconds, if specified.
    pub fn timeout(&self) -> Option<u32> {
        self.timeout
    }

    /// Returns whether to use chroot (default: true).
    pub fn use_chroot(&self) -> bool {
        self.use_chroot
    }

    /// Returns whether to use numeric IDs (default: false).
    pub fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    /// Returns whether fake super is enabled (default: false).
    pub fn fake_super(&self) -> bool {
        self.fake_super
    }

    /// Returns whether transfer logging is enabled (default: false).
    pub fn transfer_logging(&self) -> bool {
        self.transfer_logging
    }

    /// Returns the list of refused options.
    pub fn refuse_options(&self) -> &[String] {
        &self.refuse_options
    }

    /// Returns the list of file patterns that won't be compressed.
    pub fn dont_compress(&self) -> &[String] {
        &self.dont_compress
    }

    /// Returns the pre-transfer command, if specified.
    pub fn pre_xfer_exec(&self) -> Option<&str> {
        self.pre_xfer_exec.as_deref()
    }

    /// Returns the post-transfer command, if specified.
    pub fn post_xfer_exec(&self) -> Option<&str> {
        self.post_xfer_exec.as_deref()
    }
}

/// Complete daemon configuration including global parameters and modules.
#[derive(Clone, Debug, PartialEq)]
pub struct RsyncdConfig {
    global: GlobalConfig,
    modules: Vec<ModuleConfig>,
}

impl RsyncdConfig {
    /// Parses a configuration file from the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or contains invalid syntax.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path)
            .map_err(|e| ConfigError::io_error(path, e))?;
        Self::parse(&contents, path)
    }

    /// Parses configuration from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if the input contains invalid syntax.
    pub fn parse(input: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut parser = Parser::new(input, path);
        parser.parse()
    }

    /// Returns the global configuration.
    pub fn global(&self) -> &GlobalConfig {
        &self.global
    }

    /// Returns all module configurations.
    pub fn modules(&self) -> &[ModuleConfig] {
        &self.modules
    }

    /// Finds a module by name.
    pub fn get_module(&self, name: &str) -> Option<&ModuleConfig> {
        self.modules.iter().find(|m| m.name == name)
    }
}

struct Parser<'a> {
    input: &'a str,
    path: &'a Path,
    line_number: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, path: &'a Path) -> Self {
        Self {
            input,
            path,
            line_number: 0,
        }
    }

    fn parse(&mut self) -> Result<RsyncdConfig, ConfigError> {
        let mut global = GlobalConfig::default();
        let mut modules = Vec::new();
        let mut current_module: Option<ModuleBuilder> = None;
        let mut module_names = HashMap::new();

        for line in self.input.lines() {
            self.line_number += 1;
            let trimmed = line.trim();

            // Skip comments and blank lines
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }

            // Check for module header
            if trimmed.starts_with('[') {
                // Finalize previous module
                if let Some(builder) = current_module.take() {
                    let module = builder.build(self.path)?;
                    modules.push(module);
                }

                let end = trimmed.find(']')
                    .ok_or_else(|| ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "unterminated module header",
                    ))?;

                let name = trimmed[1..end].trim();
                if name.is_empty() {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "module name must be non-empty",
                    ));
                }

                // Check for duplicate modules
                if let Some(prev_line) = module_names.get(name) {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        format!("duplicate module '{}' (previously defined at line {})", name, prev_line),
                    ));
                }
                module_names.insert(name.to_string(), self.line_number);

                // Check for trailing content after ]
                let trailing = trimmed[end + 1..].trim();
                if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';') {
                    return Err(ConfigError::parse_error(
                        self.path,
                        self.line_number,
                        "unexpected content after module header",
                    ));
                }

                current_module = Some(ModuleBuilder::new(name.to_string(), self.line_number));
                continue;
            }

            // Parse key = value
            let (key, value) = line.split_once('=')
                .ok_or_else(|| ConfigError::parse_error(
                    self.path,
                    self.line_number,
                    "expected 'key = value' format",
                ))?;

            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            // Route to module or global
            if let Some(ref mut builder) = current_module {
                self.parse_module_directive(builder, &key, value)?;
            } else {
                self.parse_global_directive(&mut global, &key, value)?;
            }
        }

        // Finalize last module
        if let Some(builder) = current_module {
            let module = builder.build(self.path)?;
            modules.push(module);
        }

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
            _ => {
                // Unknown global directives are silently ignored for forward compatibility
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
            "timeout" => {
                builder.timeout = Some(value.parse().map_err(|_| {
                    ConfigError::parse_error(self.path, self.line_number, "invalid timeout value")
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
            "pre-xfer exec" => {
                builder.pre_xfer_exec = Some(value.to_string());
            }
            "post-xfer exec" => {
                builder.post_xfer_exec = Some(value.to_string());
            }
            _ => {
                // Unknown module directives are silently ignored
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
                format!("invalid boolean value '{}'", value),
            )),
        }
    }

    fn parse_list(&self, value: &str) -> Vec<String> {
        value
            .split(|c| c == ',' || c == ' ')
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
}

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
    timeout: Option<u32>,
    use_chroot: Option<bool>,
    numeric_ids: Option<bool>,
    fake_super: Option<bool>,
    transfer_logging: Option<bool>,
    refuse_options: Vec<String>,
    dont_compress: Vec<String>,
    pre_xfer_exec: Option<String>,
    post_xfer_exec: Option<String>,
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
                format!("module '{}' is missing required 'path' directive", self.name),
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
            timeout: self.timeout,
            use_chroot: self.use_chroot.unwrap_or(true),
            numeric_ids: self.numeric_ids.unwrap_or(false),
            fake_super: self.fake_super.unwrap_or(false),
            transfer_logging: self.transfer_logging.unwrap_or(false),
            refuse_options: self.refuse_options,
            dont_compress: self.dont_compress,
            pre_xfer_exec: self.pre_xfer_exec,
            post_xfer_exec: self.post_xfer_exec,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(content.as_bytes()).expect("write config");
        file.flush().expect("flush");
        file
    }

    #[test]
    fn parse_empty_config() {
        let file = write_config("");
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.global().port(), 873);
        assert!(config.modules().is_empty());
    }

    #[test]
    fn parse_global_parameters() {
        let file = write_config(
            "port = 8873\n\
             address = 127.0.0.1\n\
             motd file = /etc/motd\n\
             log file = /var/log/rsyncd.log\n\
             pid file = /var/run/rsyncd.pid\n",
        );
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.global().port(), 8873);
        assert_eq!(config.global().address(), Some("127.0.0.1"));
        assert_eq!(
            config.global().motd_file(),
            Some(Path::new("/etc/motd"))
        );
        assert_eq!(
            config.global().log_file(),
            Some(Path::new("/var/log/rsyncd.log"))
        );
        assert_eq!(
            config.global().pid_file(),
            Some(Path::new("/var/run/rsyncd.pid"))
        );
    }

    #[test]
    fn parse_minimal_module() {
        let file = write_config("[mymodule]\npath = /data/mymodule\n");
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.modules().len(), 1);

        let module = &config.modules()[0];
        assert_eq!(module.name(), "mymodule");
        assert_eq!(module.path(), Path::new("/data/mymodule"));
        assert!(module.read_only());
        assert!(!module.write_only());
        assert!(module.list());
        assert!(module.use_chroot());
        assert!(!module.numeric_ids());
    }

    #[test]
    fn parse_full_module() {
        let file = write_config(
            "[test]\n\
             path = /srv/test\n\
             comment = Test module\n\
             read only = no\n\
             write only = yes\n\
             list = no\n\
             uid = nobody\n\
             gid = nogroup\n\
             max connections = 10\n\
             lock file = /var/lock/rsync\n\
             auth users = user1, user2\n\
             secrets file = /etc/rsyncd.secrets\n\
             hosts allow = 192.168.1.0/24\n\
             hosts deny = *\n\
             exclude = .git/\n\
             include = *.txt\n\
             filter = - *.tmp\n\
             timeout = 300\n\
             use chroot = no\n\
             numeric ids = yes\n\
             fake super = yes\n\
             transfer logging = yes\n\
             refuse options = delete, hardlinks\n\
             dont compress = *.zip *.gz\n\
             pre-xfer exec = /usr/local/bin/pre-xfer\n\
             post-xfer exec = /usr/local/bin/post-xfer\n",
        );
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        let module = &config.modules()[0];

        assert_eq!(module.name(), "test");
        assert_eq!(module.path(), Path::new("/srv/test"));
        assert_eq!(module.comment(), Some("Test module"));
        assert!(!module.read_only());
        assert!(module.write_only());
        assert!(!module.list());
        assert_eq!(module.uid(), Some("nobody"));
        assert_eq!(module.gid(), Some("nogroup"));
        assert_eq!(module.max_connections(), 10);
        assert_eq!(module.lock_file(), Some(Path::new("/var/lock/rsync")));
        assert_eq!(module.auth_users(), &["user1", "user2"]);
        assert_eq!(
            module.secrets_file(),
            Some(Path::new("/etc/rsyncd.secrets"))
        );
        assert_eq!(module.hosts_allow(), &["192.168.1.0/24"]);
        assert_eq!(module.hosts_deny(), &["*"]);
        assert_eq!(module.exclude(), &[".git/"]);
        assert_eq!(module.include(), &["*.txt"]);
        assert_eq!(module.filter(), &["- *.tmp"]);
        assert_eq!(module.timeout(), Some(300));
        assert!(!module.use_chroot());
        assert!(module.numeric_ids());
        assert!(module.fake_super());
        assert!(module.transfer_logging());
        assert_eq!(module.refuse_options(), &["delete", "hardlinks"]);
        assert_eq!(module.dont_compress(), &["*.zip", "*.gz"]);
        assert_eq!(module.pre_xfer_exec(), Some("/usr/local/bin/pre-xfer"));
        assert_eq!(module.post_xfer_exec(), Some("/usr/local/bin/post-xfer"));
    }

    #[test]
    fn parse_multiple_modules() {
        let file = write_config(
            "[mod1]\npath = /data/mod1\n\n\
             [mod2]\npath = /data/mod2\ncomment = Second module\n",
        );
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.modules().len(), 2);
        assert_eq!(config.modules()[0].name(), "mod1");
        assert_eq!(config.modules()[1].name(), "mod2");
        assert_eq!(config.modules()[1].comment(), Some("Second module"));
    }

    #[test]
    fn parse_comments_and_blank_lines() {
        let file = write_config(
            "# This is a comment\n\
             ; This is also a comment\n\
             \n\
             port = 873\n\
             \n\
             [module]\n\
             # Module comment\n\
             path = /data\n",
        );
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.global().port(), 873);
        assert_eq!(config.modules().len(), 1);
    }

    #[test]
    fn parse_boolean_values() {
        for value in ["yes", "true", "1", "YES", "True"] {
            let file = write_config(&format!("[mod]\npath = /data\nread only = {}\n", value));
            let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
            assert!(config.modules()[0].read_only());
        }

        for value in ["no", "false", "0", "NO", "False"] {
            let file = write_config(&format!("[mod]\npath = /data\nread only = {}\n", value));
            let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
            assert!(!config.modules()[0].read_only());
        }
    }

    #[test]
    fn parse_list_values() {
        let file = write_config("[mod]\npath = /data\nauth users = alice, bob, charlie\n");
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(
            config.modules()[0].auth_users(),
            &["alice", "bob", "charlie"]
        );
    }

    #[test]
    fn get_module_by_name() {
        let file = write_config(
            "[first]\npath = /data/first\n\
             [second]\npath = /data/second\n",
        );
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

        assert!(config.get_module("first").is_some());
        assert!(config.get_module("second").is_some());
        assert!(config.get_module("nonexistent").is_none());
    }

    #[test]
    fn error_missing_path() {
        let file = write_config("[module]\ncomment = Missing path\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("missing required 'path'"));
        assert_eq!(err.line(), Some(1));
    }

    #[test]
    fn error_unterminated_header() {
        let file = write_config("[module\npath = /data\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("unterminated module header"));
        assert_eq!(err.line(), Some(1));
    }

    #[test]
    fn error_empty_module_name() {
        let file = write_config("[]\npath = /data\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("module name must be non-empty"));
    }

    #[test]
    fn error_invalid_boolean() {
        let file = write_config("[mod]\npath = /data\nread only = maybe\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid boolean"));
    }

    #[test]
    fn error_invalid_port() {
        let file = write_config("port = not_a_number\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid port"));
    }

    #[test]
    fn error_duplicate_module() {
        let file = write_config(
            "[module]\npath = /data/one\n\
             [module]\npath = /data/two\n",
        );
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate module"));
    }

    #[test]
    fn error_missing_equals() {
        let file = write_config("[mod]\npath /data\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("key = value"));
    }

    #[test]
    fn error_empty_path() {
        let file = write_config("[mod]\npath = \n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("path must not be empty"));
    }

    #[test]
    fn error_empty_secrets_file() {
        let file = write_config("[mod]\npath = /data\nsecrets file = \n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("secrets file must not be empty"));
    }

    #[test]
    fn trailing_comment_after_header() {
        let file = write_config("[module] # This is a comment\npath = /data\n");
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.modules().len(), 1);
    }

    #[test]
    fn trailing_text_after_header_errors() {
        let file = write_config("[module] extra text\npath = /data\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("unexpected content"));
    }

    #[test]
    fn case_insensitive_keys() {
        let file = write_config("[MOD]\nPATH = /data\nREAD ONLY = NO\n");
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        assert_eq!(config.modules()[0].path(), Path::new("/data"));
        assert!(!config.modules()[0].read_only());
    }

    #[test]
    fn default_values() {
        let file = write_config("[mod]\npath = /data\n");
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");
        let module = &config.modules()[0];

        assert!(module.read_only());
        assert!(!module.write_only());
        assert!(module.list());
        assert_eq!(module.max_connections(), 0);
        assert!(module.use_chroot());
        assert!(!module.numeric_ids());
        assert!(!module.fake_super());
        assert!(!module.transfer_logging());
        assert!(module.auth_users().is_empty());
        assert!(module.refuse_options().is_empty());
        assert!(module.dont_compress().is_empty());
    }

    #[test]
    fn config_error_display() {
        let file = write_config("[mod]\npath =\n");
        let err = RsyncdConfig::from_file(file.path()).expect_err("should fail");
        let display = err.to_string();
        assert!(display.contains("line 2"));
        assert!(display.contains("path must not be empty"));
    }

    #[test]
    fn parse_with_global_and_modules() {
        let file = write_config(
            "port = 8873\n\
             log file = /var/log/rsync.log\n\
             \n\
             [public]\n\
             path = /srv/public\n\
             comment = Public files\n\
             read only = yes\n\
             \n\
             [upload]\n\
             path = /srv/upload\n\
             comment = Upload area\n\
             read only = no\n",
        );
        let config = RsyncdConfig::from_file(file.path()).expect("parse succeeds");

        assert_eq!(config.global().port(), 8873);
        assert_eq!(
            config.global().log_file(),
            Some(Path::new("/var/log/rsync.log"))
        );
        assert_eq!(config.modules().len(), 2);

        let public = config.get_module("public").unwrap();
        assert_eq!(public.comment(), Some("Public files"));
        assert!(public.read_only());

        let upload = config.get_module("upload").unwrap();
        assert_eq!(upload.comment(), Some("Upload area"));
        assert!(!upload.read_only());
    }
}
