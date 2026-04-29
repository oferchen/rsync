//! SSH configuration for the embedded transport.
//!
//! Provides `SshConfig` with sensible defaults and a builder API for
//! programmatic construction, plus `from_url()` for parsing `ssh://` URLs.

use std::path::PathBuf;
use std::time::Duration;

use url::Url;

use super::error::SshError;
use super::types::{IpPreference, StrictHostKeyChecking};

/// Default SSH port.
const DEFAULT_PORT: u16 = 22;

/// Default connection timeout in seconds.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Default keepalive interval in seconds.
const DEFAULT_KEEPALIVE_INTERVAL_SECS: u64 = 60;

/// Default keepalive max count before disconnect.
const DEFAULT_KEEPALIVE_MAX_COUNT: u32 = 3;

/// Configuration for an embedded SSH connection.
///
/// Holds all parameters needed to establish and maintain an SSH session.
/// Use `Default::default()` for sensible defaults or the builder methods
/// to customize individual fields. For `ssh://` URLs, use `from_url()`.
///
/// # Examples
///
/// Builder-style construction:
///
/// ```no_run
/// use rsync_io::ssh::embedded::SshConfig;
/// use std::time::Duration;
///
/// let mut cfg = SshConfig::default();
/// cfg.host("example.com")
///    .port(2222)
///    .username("deploy")
///    .connect_timeout(Duration::from_secs(10));
/// ```
///
/// Parsing from an SSH URL:
///
/// ```no_run
/// use rsync_io::ssh::embedded::SshConfig;
///
/// let (cfg, remote_path) = SshConfig::from_url("ssh://user@host/~/data").unwrap();
/// assert_eq!(cfg.host, "host");
/// assert_eq!(remote_path, "~/data");
/// ```
#[derive(Debug, Clone)]
pub struct SshConfig {
    /// Remote hostname or IP address.
    pub host: String,
    /// Remote port number.
    pub port: u16,
    /// Username for authentication. `None` defers to the system default.
    pub username: Option<String>,
    /// Password for password-based authentication.
    pub password: Option<String>,
    /// Paths to SSH private key files, tried in order.
    pub identity_files: Vec<PathBuf>,
    /// Whether to attempt authentication via an SSH agent.
    pub use_agent: bool,
    /// Cipher preference list. `None` uses hardware-detected defaults.
    pub ciphers: Option<Vec<String>>,
    /// TCP connect timeout.
    pub connect_timeout: Duration,
    /// Interval between keepalive packets. `None` disables keepalives.
    pub keepalive_interval: Option<Duration>,
    /// Number of missed keepalives before disconnecting.
    pub keepalive_max_count: u32,
    /// Path to the known hosts file. `None` disables host key storage.
    pub known_hosts_file: Option<PathBuf>,
    /// Host key verification policy.
    pub strict_host_key_checking: StrictHostKeyChecking,
    /// IP version preference for DNS resolution.
    pub ip_preference: IpPreference,
}

/// Returns the default identity file paths under `~/.ssh/`.
fn default_identity_files() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let ssh_dir = home.join(".ssh");
    vec![
        ssh_dir.join("id_ed25519"),
        ssh_dir.join("id_rsa"),
        ssh_dir.join("id_ecdsa"),
    ]
}

/// Returns the default known hosts file path.
fn default_known_hosts_file() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// Cross-platform home directory lookup.
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_PORT,
            username: None,
            password: None,
            identity_files: default_identity_files(),
            use_agent: true,
            ciphers: None,
            connect_timeout: Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
            keepalive_interval: Some(Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS)),
            keepalive_max_count: DEFAULT_KEEPALIVE_MAX_COUNT,
            known_hosts_file: default_known_hosts_file(),
            strict_host_key_checking: StrictHostKeyChecking::default(),
            ip_preference: IpPreference::default(),
        }
    }
}

impl SshConfig {
    /// Sets the remote hostname or IP address.
    pub fn host(&mut self, host: impl Into<String>) -> &mut Self {
        self.host = host.into();
        self
    }

    /// Sets the remote port number.
    pub fn port(&mut self, port: u16) -> &mut Self {
        self.port = port;
        self
    }

    /// Sets the username for authentication.
    pub fn username(&mut self, username: impl Into<String>) -> &mut Self {
        self.username = Some(username.into());
        self
    }

    /// Sets the password for password-based authentication.
    pub fn password(&mut self, password: impl Into<String>) -> &mut Self {
        self.password = Some(password.into());
        self
    }

    /// Sets the identity file paths to try during key-based authentication.
    pub fn identity_files(&mut self, files: Vec<PathBuf>) -> &mut Self {
        self.identity_files = files;
        self
    }

    /// Sets whether to attempt SSH agent authentication.
    pub fn use_agent(&mut self, use_agent: bool) -> &mut Self {
        self.use_agent = use_agent;
        self
    }

    /// Sets the cipher preference list. Pass `None` for hardware-detected defaults.
    pub fn ciphers(&mut self, ciphers: Option<Vec<String>>) -> &mut Self {
        self.ciphers = ciphers;
        self
    }

    /// Sets the TCP connection timeout.
    pub fn connect_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the keepalive interval. Pass `None` to disable keepalives.
    pub fn keepalive_interval(&mut self, interval: Option<Duration>) -> &mut Self {
        self.keepalive_interval = interval;
        self
    }

    /// Sets the maximum number of missed keepalives before disconnecting.
    pub fn keepalive_max_count(&mut self, count: u32) -> &mut Self {
        self.keepalive_max_count = count;
        self
    }

    /// Sets the path to the known hosts file. Pass `None` to disable host key storage.
    pub fn known_hosts_file(&mut self, path: Option<PathBuf>) -> &mut Self {
        self.known_hosts_file = path;
        self
    }

    /// Sets the host key verification policy.
    pub fn strict_host_key_checking(&mut self, policy: StrictHostKeyChecking) -> &mut Self {
        self.strict_host_key_checking = policy;
        self
    }

    /// Sets the IP version preference for DNS resolution.
    pub fn ip_preference(&mut self, pref: IpPreference) -> &mut Self {
        self.ip_preference = pref;
        self
    }

    /// Parses an `ssh://` URL into an `SshConfig` and remote path.
    ///
    /// Accepted formats:
    /// - `ssh://host/path`
    /// - `ssh://user@host/path`
    /// - `ssh://user:password@host/path`
    /// - `ssh://host:2222/path`
    /// - `ssh://user@[::1]:22/path` (IPv6 bracket notation)
    /// - `ssh://user@host/~/relative` (home-relative path)
    ///
    /// Returns `(config, remote_path)` where `remote_path` is the decoded path
    /// component. A leading `/~/` is converted to `~/` for home-relative paths.
    /// Fields not present in the URL (ciphers, timeouts, etc.) use `Default`
    /// values.
    ///
    /// # Errors
    ///
    /// Returns `SshError::UrlParse` for malformed URLs, or `SshError::InvalidUrl`
    /// when the scheme is not `ssh://` or the host/path is empty.
    pub fn from_url(url_str: &str) -> Result<(Self, String), SshError> {
        let parsed = Url::parse(url_str)?;

        if parsed.scheme() != "ssh" {
            return Err(SshError::InvalidUrl {
                reason: format!("expected ssh:// scheme, got {}://", parsed.scheme()),
            });
        }

        let raw_host = parsed.host_str().ok_or_else(|| SshError::InvalidUrl {
            reason: "missing host".to_owned(),
        })?;
        if raw_host.is_empty() {
            return Err(SshError::InvalidUrl {
                reason: "empty host".to_owned(),
            });
        }
        // Strip surrounding brackets from IPv6 addresses.
        let host = raw_host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(raw_host);

        let raw_path = parsed.path();
        if raw_path.is_empty() || raw_path == "/" {
            return Err(SshError::InvalidUrl {
                reason: "empty path".to_owned(),
            });
        }

        // Strip the leading slash that the URL parser always includes.
        let path = &raw_path[1..];

        // Convert ~/... to a home-relative path marker.
        let remote_path = if let Some(rest) = path.strip_prefix("~/") {
            format!("~/{rest}")
        } else if path == "~" {
            "~".to_owned()
        } else {
            format!("/{path}")
        };

        let port = parsed.port().unwrap_or(DEFAULT_PORT);
        let username = {
            let user = parsed.username();
            if user.is_empty() {
                None
            } else {
                Some(user.to_owned())
            }
        };
        let password = parsed.password().map(str::to_owned);

        let config = Self {
            host: host.to_owned(),
            port,
            username,
            password,
            ..Self::default()
        };

        Ok((config, remote_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_22() {
        let cfg = SshConfig::default();
        assert_eq!(cfg.port, 22);
    }

    #[test]
    fn default_host_is_empty() {
        let cfg = SshConfig::default();
        assert!(cfg.host.is_empty());
    }

    #[test]
    fn default_username_is_none() {
        let cfg = SshConfig::default();
        assert!(cfg.username.is_none());
    }

    #[test]
    fn default_password_is_none() {
        let cfg = SshConfig::default();
        assert!(cfg.password.is_none());
    }

    #[test]
    fn default_use_agent_is_true() {
        let cfg = SshConfig::default();
        assert!(cfg.use_agent);
    }

    #[test]
    fn default_ciphers_is_none() {
        let cfg = SshConfig::default();
        assert!(cfg.ciphers.is_none());
    }

    #[test]
    fn default_connect_timeout_is_30s() {
        let cfg = SshConfig::default();
        assert_eq!(cfg.connect_timeout, Duration::from_secs(30));
    }

    #[test]
    fn default_keepalive_interval_is_60s() {
        let cfg = SshConfig::default();
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(60)));
    }

    #[test]
    fn default_keepalive_max_count_is_3() {
        let cfg = SshConfig::default();
        assert_eq!(cfg.keepalive_max_count, 3);
    }

    #[test]
    fn default_strict_host_key_checking_is_ask() {
        let cfg = SshConfig::default();
        assert_eq!(cfg.strict_host_key_checking, StrictHostKeyChecking::Ask);
    }

    #[test]
    fn default_ip_preference_is_auto() {
        let cfg = SshConfig::default();
        assert_eq!(cfg.ip_preference, IpPreference::Auto);
    }

    #[test]
    fn default_identity_files_contains_ed25519() {
        let cfg = SshConfig::default();
        assert!(cfg.identity_files.iter().any(|p| p.ends_with("id_ed25519")));
    }

    #[test]
    fn default_identity_files_contains_rsa() {
        let cfg = SshConfig::default();
        assert!(cfg.identity_files.iter().any(|p| p.ends_with("id_rsa")));
    }

    #[test]
    fn default_identity_files_contains_ecdsa() {
        let cfg = SshConfig::default();
        assert!(cfg.identity_files.iter().any(|p| p.ends_with("id_ecdsa")));
    }

    #[test]
    fn default_known_hosts_file_ends_with_known_hosts() {
        let cfg = SshConfig::default();
        if let Some(path) = &cfg.known_hosts_file {
            assert!(path.ends_with("known_hosts"));
        }
    }

    #[test]
    fn builder_host() {
        let mut cfg = SshConfig::default();
        cfg.host("example.com");
        assert_eq!(cfg.host, "example.com");
    }

    #[test]
    fn builder_port() {
        let mut cfg = SshConfig::default();
        cfg.port(2222);
        assert_eq!(cfg.port, 2222);
    }

    #[test]
    fn builder_username() {
        let mut cfg = SshConfig::default();
        cfg.username("alice");
        assert_eq!(cfg.username.as_deref(), Some("alice"));
    }

    #[test]
    fn builder_password() {
        let mut cfg = SshConfig::default();
        cfg.password("secret");
        assert_eq!(cfg.password.as_deref(), Some("secret"));
    }

    #[test]
    fn builder_use_agent() {
        let mut cfg = SshConfig::default();
        cfg.use_agent(false);
        assert!(!cfg.use_agent);
    }

    #[test]
    fn builder_ciphers() {
        let mut cfg = SshConfig::default();
        cfg.ciphers(Some(vec!["aes128-ctr".to_owned()]));
        assert_eq!(cfg.ciphers.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn builder_connect_timeout() {
        let mut cfg = SshConfig::default();
        cfg.connect_timeout(Duration::from_secs(10));
        assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
    }

    #[test]
    fn builder_keepalive_interval_none() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_interval(None);
        assert!(cfg.keepalive_interval.is_none());
    }

    #[test]
    fn builder_keepalive_max_count() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_max_count(5);
        assert_eq!(cfg.keepalive_max_count, 5);
    }

    #[test]
    fn builder_known_hosts_file() {
        let mut cfg = SshConfig::default();
        cfg.known_hosts_file(Some(PathBuf::from("/tmp/known_hosts")));
        assert_eq!(
            cfg.known_hosts_file,
            Some(PathBuf::from("/tmp/known_hosts"))
        );
    }

    #[test]
    fn builder_strict_host_key_checking() {
        let mut cfg = SshConfig::default();
        cfg.strict_host_key_checking(StrictHostKeyChecking::Yes);
        assert_eq!(cfg.strict_host_key_checking, StrictHostKeyChecking::Yes);
    }

    #[test]
    fn builder_ip_preference() {
        let mut cfg = SshConfig::default();
        cfg.ip_preference(IpPreference::ForceV6);
        assert_eq!(cfg.ip_preference, IpPreference::ForceV6);
    }

    #[test]
    fn builder_identity_files() {
        let mut cfg = SshConfig::default();
        cfg.identity_files(vec![PathBuf::from("/tmp/mykey")]);
        assert_eq!(cfg.identity_files, vec![PathBuf::from("/tmp/mykey")]);
    }

    #[test]
    fn builder_chaining() {
        let mut cfg = SshConfig::default();
        cfg.host("example.com").port(2222).username("bob");
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.username.as_deref(), Some("bob"));
    }

    #[test]
    fn from_url_simple_host_path() {
        let (cfg, path) = SshConfig::from_url("ssh://host/path/to/file").unwrap();
        assert_eq!(cfg.host, "host");
        assert_eq!(cfg.port, 22);
        assert!(cfg.username.is_none());
        assert!(cfg.password.is_none());
        assert_eq!(path, "/path/to/file");
    }

    #[test]
    fn from_url_with_user() {
        let (cfg, path) = SshConfig::from_url("ssh://user@host/path").unwrap();
        assert_eq!(cfg.host, "host");
        assert_eq!(cfg.username.as_deref(), Some("user"));
        assert!(cfg.password.is_none());
        assert_eq!(path, "/path");
    }

    #[test]
    fn from_url_with_user_and_password() {
        let (cfg, path) = SshConfig::from_url("ssh://user:pass@host/path").unwrap();
        assert_eq!(cfg.host, "host");
        assert_eq!(cfg.username.as_deref(), Some("user"));
        assert_eq!(cfg.password.as_deref(), Some("pass"));
        assert_eq!(path, "/path");
    }

    #[test]
    fn from_url_with_port() {
        let (cfg, path) = SshConfig::from_url("ssh://host:2222/path").unwrap();
        assert_eq!(cfg.host, "host");
        assert_eq!(cfg.port, 2222);
        assert_eq!(path, "/path");
    }

    #[test]
    fn from_url_ipv6_bracket() {
        let (cfg, path) = SshConfig::from_url("ssh://user@[::1]:22/path").unwrap();
        assert_eq!(cfg.host, "::1");
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.username.as_deref(), Some("user"));
        assert_eq!(path, "/path");
    }

    #[test]
    fn from_url_ipv6_no_port() {
        let (cfg, path) = SshConfig::from_url("ssh://[::1]/data").unwrap();
        assert_eq!(cfg.host, "::1");
        assert_eq!(cfg.port, 22);
        assert_eq!(path, "/data");
    }

    #[test]
    fn from_url_home_relative() {
        let (cfg, path) = SshConfig::from_url("ssh://user@host/~/relative/path").unwrap();
        assert_eq!(cfg.host, "host");
        assert_eq!(path, "~/relative/path");
    }

    #[test]
    fn from_url_home_only() {
        let (_cfg, path) = SshConfig::from_url("ssh://host/~").unwrap();
        assert_eq!(path, "~");
    }

    #[test]
    fn from_url_absolute_path() {
        let (_cfg, path) = SshConfig::from_url("ssh://host/absolute/path").unwrap();
        assert_eq!(path, "/absolute/path");
    }

    #[test]
    fn from_url_error_empty_string() {
        let result = SshConfig::from_url("");
        assert!(result.is_err());
    }

    #[test]
    fn from_url_error_non_ssh_scheme() {
        let result = SshConfig::from_url("http://host/path");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected ssh://"), "got: {msg}");
    }

    #[test]
    fn from_url_error_missing_host() {
        let result = SshConfig::from_url("ssh:///path");
        assert!(result.is_err());
    }

    #[test]
    fn from_url_error_empty_path() {
        let result = SshConfig::from_url("ssh://host");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("empty path"), "got: {msg}");
    }

    #[test]
    fn from_url_error_slash_only_path() {
        let result = SshConfig::from_url("ssh://host/");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("empty path"), "got: {msg}");
    }

    #[test]
    fn from_url_ipv4_address() {
        let (cfg, path) = SshConfig::from_url("ssh://192.168.1.1/data").unwrap();
        assert_eq!(cfg.host, "192.168.1.1");
        assert_eq!(path, "/data");
    }

    #[test]
    fn from_url_full_featured() {
        let (cfg, path) =
            SshConfig::from_url("ssh://admin:s3cret@server.example.com:2222/~/backups").unwrap();
        assert_eq!(cfg.host, "server.example.com");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.username.as_deref(), Some("admin"));
        assert_eq!(cfg.password.as_deref(), Some("s3cret"));
        assert_eq!(path, "~/backups");
    }

    #[test]
    fn connect_timeout_zero_disables_timeout() {
        let mut cfg = SshConfig::default();
        cfg.connect_timeout(Duration::ZERO);
        assert_eq!(cfg.connect_timeout, Duration::ZERO);
    }

    #[test]
    fn connect_timeout_large_value() {
        let mut cfg = SshConfig::default();
        let one_day = Duration::from_secs(86_400);
        cfg.connect_timeout(one_day);
        assert_eq!(cfg.connect_timeout, one_day);
    }

    #[test]
    fn keepalive_interval_custom_value() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_interval(Some(Duration::from_secs(15)));
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(15)));
    }

    #[test]
    fn keepalive_interval_sub_second() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_interval(Some(Duration::from_millis(500)));
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_millis(500)));
    }

    #[test]
    fn keepalive_max_count_zero() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_max_count(0);
        assert_eq!(cfg.keepalive_max_count, 0);
    }

    #[test]
    fn keepalive_max_count_large_value() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_max_count(u32::MAX);
        assert_eq!(cfg.keepalive_max_count, u32::MAX);
    }

    #[test]
    fn builder_chaining_timeout_and_keepalive() {
        let mut cfg = SshConfig::default();
        cfg.connect_timeout(Duration::from_secs(5))
            .keepalive_interval(Some(Duration::from_secs(10)))
            .keepalive_max_count(7);
        assert_eq!(cfg.connect_timeout, Duration::from_secs(5));
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(10)));
        assert_eq!(cfg.keepalive_max_count, 7);
    }

    #[test]
    fn builder_disable_keepalive_then_reenable() {
        let mut cfg = SshConfig::default();
        cfg.keepalive_interval(None);
        assert!(cfg.keepalive_interval.is_none());
        cfg.keepalive_interval(Some(Duration::from_secs(30)));
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(30)));
    }

    #[test]
    fn timeout_round_trip_preserves_nanos() {
        let mut cfg = SshConfig::default();
        let precise = Duration::new(10, 123_456_789);
        cfg.connect_timeout(precise);
        assert_eq!(cfg.connect_timeout, precise);
    }

    #[test]
    fn from_url_preserves_defaults_for_non_url_fields() {
        let (cfg, _) = SshConfig::from_url("ssh://host/path").unwrap();
        let defaults = SshConfig::default();
        assert_eq!(cfg.use_agent, defaults.use_agent);
        assert_eq!(cfg.ciphers, defaults.ciphers);
        assert_eq!(cfg.connect_timeout, defaults.connect_timeout);
        assert_eq!(cfg.keepalive_interval, defaults.keepalive_interval);
        assert_eq!(cfg.keepalive_max_count, defaults.keepalive_max_count);
        assert_eq!(
            cfg.strict_host_key_checking,
            defaults.strict_host_key_checking
        );
        assert_eq!(cfg.ip_preference, defaults.ip_preference);
    }
}
