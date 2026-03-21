use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use super::AuthUser;
// HostPattern is defined in the parent daemon module (via include!() of config_helpers.rs).
use crate::daemon::HostPattern;

/// Configuration for a single rsync module.
///
/// A module represents a named filesystem path that can be accessed via rsync daemon.
/// Each module has its own access controls, bandwidth limits, and metadata handling options.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModuleDefinition {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) comment: Option<String>,
    pub(crate) hosts_allow: Vec<HostPattern>,
    pub(crate) hosts_deny: Vec<HostPattern>,
    pub(crate) auth_users: Vec<AuthUser>,
    pub(crate) secrets_file: Option<PathBuf>,
    pub(crate) bandwidth_limit: Option<NonZeroU64>,
    pub(crate) bandwidth_limit_specified: bool,
    pub(crate) bandwidth_burst: Option<NonZeroU64>,
    pub(crate) bandwidth_burst_specified: bool,
    pub(crate) bandwidth_limit_configured: bool,
    pub(crate) refuse_options: Vec<String>,
    pub(crate) read_only: bool,
    pub(crate) write_only: bool,
    pub(crate) numeric_ids: bool,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<u32>,
    pub(crate) timeout: Option<NonZeroU64>,
    pub(crate) listable: bool,
    pub(crate) use_chroot: bool,
    pub(crate) max_connections: Option<NonZeroU32>,
    pub(crate) incoming_chmod: Option<String>,
    pub(crate) outgoing_chmod: Option<String>,
    /// When true, stores privileged metadata (uid/gid, devices) in xattrs instead of applying.
    ///
    /// This mirrors upstream rsync's `fake super` directive from `rsyncd.conf(5)`.
    /// Enables backup/restore operations without root privileges by storing ownership
    /// and special file metadata in the `user.rsync.%stat` extended attribute.
    pub(crate) fake_super: bool,
    /// Controls whether symlink targets are munged with a `/rsyncd-munged/` prefix.
    ///
    /// When `None`, the effective value defaults to `!use_chroot` (upstream behaviour).
    /// When `Some(true)`, munging is always enabled. When `Some(false)`, munging is
    /// always disabled regardless of chroot setting.
    ///
    /// Upstream: `clientserver.c` - `munge_symlinks` global; defaults to true when
    /// `use_chroot` is false or when an inside-chroot module is configured.
    pub(crate) munge_symlinks: Option<bool>,
    /// Caps the client's requested verbosity level.
    ///
    /// Upstream: `loadparm.c` - `max verbosity` parameter, default 1.
    pub(crate) max_verbosity: i32,
    /// When true, I/O errors during delete operations are ignored.
    ///
    /// Upstream: `loadparm.c` - `ignore errors` parameter, default false.
    pub(crate) ignore_errors: bool,
    /// When true, files the daemon cannot read are silently skipped.
    ///
    /// Upstream: `loadparm.c` - `ignore nonreadable` parameter, default false.
    pub(crate) ignore_nonreadable: bool,
    /// When true, per-file transfer logging is enabled.
    ///
    /// Upstream: `loadparm.c` - `transfer logging` parameter, default false.
    pub(crate) transfer_logging: bool,
    /// Format string for per-file transfer log entries.
    ///
    /// Upstream: `loadparm.c` - `log format` parameter, default `"%o %h [%a] %m (%u) %f %l"`.
    pub(crate) log_format: Option<String>,
    /// Per-module log file path.
    /// upstream: daemon-parm.h - `log file` STRING, P_LOCAL.
    pub(crate) log_file: Option<PathBuf>,
    /// Glob patterns of files that should not be compressed during transfer.
    ///
    /// Upstream: `loadparm.c` - `dont compress` parameter.
    pub(crate) dont_compress: Option<String>,
    /// Command to execute early in the connection, before file list exchange.
    ///
    /// Upstream: `loadparm.c` - `early exec` parameter. Runs after module selection
    /// but before argument exchange and authentication.
    pub(crate) early_exec: Option<String>,
    /// Command to execute before a transfer begins.
    ///
    /// Upstream: `loadparm.c` - `pre-xfer exec` parameter.
    pub(crate) pre_xfer_exec: Option<String>,
    /// Command to execute after a transfer completes.
    ///
    /// Upstream: `loadparm.c` - `post-xfer exec` parameter.
    pub(crate) post_xfer_exec: Option<String>,
    /// External program for mapping user/group names to IDs and vice versa.
    ///
    /// Used when the daemon runs in a chroot where `/etc/passwd` is unavailable.
    /// Upstream: `loadparm.c` - `name converter` parameter.
    pub(crate) name_converter: Option<String>,
    /// Temporary directory for receiving files before final placement.
    ///
    /// Upstream: `loadparm.c` - `temp dir` parameter.
    pub(crate) temp_dir: Option<String>,
    /// Character set for filename conversion.
    ///
    /// Upstream: `loadparm.c` - `charset` parameter.
    pub(crate) charset: Option<String>,
    /// When true, DNS forward lookup verification is performed on connecting hosts.
    ///
    /// Upstream: `loadparm.c` - `forward lookup` parameter, default true.
    pub(crate) forward_lookup: bool,
    /// When true, the daemon checks that the secrets file has appropriate permissions.
    ///
    /// Upstream: `loadparm.c` - `strict modes` parameter, default true.
    /// Controls whether the secrets file must not be world-readable.
    pub(crate) strict_modes: bool,
    /// Path to a file containing exclude patterns for this module.
    ///
    /// Upstream: `daemon-parm.txt` - `exclude_from` STRING parameter, default NULL.
    /// Patterns are loaded via `parse_filter_file()` in `clientserver.c`.
    pub(crate) exclude_from: Option<PathBuf>,
    /// Path to a file containing include patterns for this module.
    ///
    /// Upstream: `daemon-parm.txt` - `include_from` STRING parameter, default NULL.
    /// Patterns are loaded via `parse_filter_file()` in `clientserver.c`.
    pub(crate) include_from: Option<PathBuf>,
    /// Direct filter rules for this module (space-separated, repeatable).
    ///
    /// Upstream: `daemon-parm.h` - `filter` STRING, P_LOCAL.
    /// Parsed with `FILTRULE_WORD_SPLIT` in `clientserver.c:rsync_module()`.
    pub(crate) filter: Vec<String>,
    /// Direct exclude rules for this module (space-separated, repeatable).
    ///
    /// Upstream: `daemon-parm.h` - `exclude` STRING, P_LOCAL.
    /// Parsed with `FILTRULE_WORD_SPLIT` in `clientserver.c:rsync_module()`.
    pub(crate) exclude: Vec<String>,
    /// Direct include rules for this module (space-separated, repeatable).
    ///
    /// Upstream: `daemon-parm.h` - `include` STRING, P_LOCAL.
    /// Parsed with `FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT` in `clientserver.c:rsync_module()`.
    pub(crate) include: Vec<String>,
    /// When true, source files are opened with `O_NOATIME` to avoid updating access times.
    ///
    /// Only effective on Linux where `O_NOATIME` is supported. On other platforms this
    /// is accepted but has no effect (no-op).
    /// Upstream: `loadparm.c` - `open noatime` parameter, default false.
    pub(crate) open_noatime: bool,
}

impl ModuleDefinition {
    /// Checks whether a peer is permitted to access this module.
    pub(crate) fn permits(&self, addr: std::net::IpAddr, hostname: Option<&str>) -> bool {
        if !self.hosts_allow.is_empty()
            && !self
                .hosts_allow
                .iter()
                .any(|pattern| pattern.matches(addr, hostname))
        {
            return false;
        }

        if self
            .hosts_deny
            .iter()
            .any(|pattern| pattern.matches(addr, hostname))
        {
            return false;
        }

        true
    }

    /// Returns whether any host pattern requires DNS hostname resolution.
    pub(in crate::daemon) fn requires_hostname_lookup(&self) -> bool {
        self.hosts_allow
            .iter()
            .chain(self.hosts_deny.iter())
            .any(HostPattern::requires_hostname)
    }

    /// Returns whether this module requires user authentication.
    pub(in crate::daemon) fn requires_authentication(&self) -> bool {
        !self.auth_users.is_empty()
    }

    /// Returns the AuthUser if the username is authorized for this module.
    pub(crate) fn get_auth_user(&self, username: &str) -> Option<&AuthUser> {
        self.auth_users
            .iter()
            .find(|auth| auth.username == username)
    }

    /// Returns the maximum number of concurrent connections allowed.
    pub(crate) const fn max_connections(&self) -> Option<NonZeroU32> {
        self.max_connections
    }

    /// Returns the configured bandwidth limit in bytes per second.
    pub(crate) const fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    /// Returns whether the bandwidth limit was explicitly specified.
    pub(crate) const fn bandwidth_limit_specified(&self) -> bool {
        self.bandwidth_limit_specified
    }

    /// Returns the configured bandwidth burst in bytes.
    pub(crate) const fn bandwidth_burst(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    /// Returns whether the bandwidth burst was explicitly specified.
    pub(crate) const fn bandwidth_burst_specified(&self) -> bool {
        self.bandwidth_burst_specified
    }

    /// Returns whether any bandwidth limit is configured for this module.
    pub(in crate::daemon) const fn bandwidth_limit_configured(&self) -> bool {
        self.bandwidth_limit_configured
    }

    /// Inherits refuse options from the global config if none are set locally.
    pub(in crate::daemon) fn inherit_refuse_options(&mut self, options: &[String]) {
        if self.refuse_options.is_empty() {
            self.refuse_options = options.to_vec();
        }
    }

    /// Returns whether symlink munging is effective for this module.
    ///
    /// When `munge_symlinks` is `None` (auto), defaults to `!use_chroot`.
    /// Upstream: `clientserver.c` sets `munge_symlinks = !use_chroot` before reading
    /// the per-module override.
    #[allow(dead_code)] // Wired during file list send/receive in daemon mode
    pub(crate) fn effective_munge_symlinks(&self) -> bool {
        self.munge_symlinks.unwrap_or(!self.use_chroot)
    }

    /// Returns the per-module log file path, if configured.
    #[allow(dead_code)]
    pub(crate) fn module_log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Inherits incoming chmod from the global config if none is set locally.
    pub(in crate::daemon) fn inherit_incoming_chmod(&mut self, chmod: Option<&str>) {
        if self.incoming_chmod.is_none() {
            self.incoming_chmod = chmod.map(str::to_string);
        }
    }

    /// Inherits outgoing chmod from the global config if none is set locally.
    pub(in crate::daemon) fn inherit_outgoing_chmod(&mut self, chmod: Option<&str>) {
        if self.outgoing_chmod.is_none() {
            self.outgoing_chmod = chmod.map(str::to_string);
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl ModuleDefinition {
    /// Returns the list of authorized users.
    pub(crate) fn auth_users(&self) -> &[AuthUser] {
        &self.auth_users
    }

    /// Returns the secrets file path.
    pub(crate) fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }

    /// Returns the module name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the list of refused options.
    pub(crate) fn refused_options(&self) -> &[String] {
        &self.refuse_options
    }

    /// Returns whether the module is read-only.
    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    /// Returns whether the module is write-only.
    pub(crate) fn write_only(&self) -> bool {
        self.write_only
    }

    /// Returns whether numeric IDs are enabled.
    pub(crate) fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    /// Returns the configured UID override.
    pub(crate) fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the configured GID override.
    pub(crate) fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the configured timeout in seconds.
    pub(crate) fn timeout(&self) -> Option<NonZeroU64> {
        self.timeout
    }

    /// Returns whether the module is listable.
    pub(crate) fn listable(&self) -> bool {
        self.listable
    }

    /// Returns whether chroot is enabled.
    pub(crate) fn use_chroot(&self) -> bool {
        self.use_chroot
    }

    /// Returns the incoming chmod string.
    pub(crate) fn incoming_chmod(&self) -> Option<&str> {
        self.incoming_chmod.as_deref()
    }

    /// Returns the outgoing chmod string.
    pub(crate) fn outgoing_chmod(&self) -> Option<&str> {
        self.outgoing_chmod.as_deref()
    }

    /// Returns whether fake super is enabled.
    pub(crate) fn fake_super(&self) -> bool {
        self.fake_super
    }

    /// Returns the munge symlinks override setting.
    pub(crate) fn munge_symlinks(&self) -> Option<bool> {
        self.munge_symlinks
    }

    /// Returns the max verbosity cap.
    pub(crate) fn max_verbosity(&self) -> i32 {
        self.max_verbosity
    }

    /// Returns whether I/O errors during delete are ignored.
    pub(crate) fn ignore_errors(&self) -> bool {
        self.ignore_errors
    }

    /// Returns whether non-readable files are silently skipped.
    pub(crate) fn ignore_nonreadable(&self) -> bool {
        self.ignore_nonreadable
    }

    /// Returns whether transfer logging is enabled.
    pub(crate) fn transfer_logging(&self) -> bool {
        self.transfer_logging
    }

    /// Returns the log format string.
    pub(crate) fn log_format(&self) -> Option<&str> {
        self.log_format.as_deref()
    }

    /// Returns the log file path.
    pub(crate) fn log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Returns the dont-compress glob patterns.
    pub(crate) fn dont_compress(&self) -> Option<&str> {
        self.dont_compress.as_deref()
    }

    /// Returns the early exec command.
    pub(crate) fn early_exec(&self) -> Option<&str> {
        self.early_exec.as_deref()
    }

    /// Returns the pre-transfer exec command.
    pub(crate) fn pre_xfer_exec(&self) -> Option<&str> {
        self.pre_xfer_exec.as_deref()
    }

    /// Returns the post-transfer exec command.
    pub(crate) fn post_xfer_exec(&self) -> Option<&str> {
        self.post_xfer_exec.as_deref()
    }

    /// Returns the name converter program path.
    pub(crate) fn name_converter(&self) -> Option<&str> {
        self.name_converter.as_deref()
    }

    /// Returns the temp directory path.
    pub(crate) fn temp_dir(&self) -> Option<&str> {
        self.temp_dir.as_deref()
    }

    /// Returns the charset for filename conversion.
    pub(crate) fn charset(&self) -> Option<&str> {
        self.charset.as_deref()
    }

    /// Returns whether forward DNS lookup is enabled.
    pub(crate) fn forward_lookup(&self) -> bool {
        self.forward_lookup
    }

    /// Returns whether strict modes are enabled for secrets file.
    pub(crate) fn strict_modes(&self) -> bool {
        self.strict_modes
    }

    /// Returns the exclude-from file path.
    pub(crate) fn exclude_from(&self) -> Option<&Path> {
        self.exclude_from.as_deref()
    }

    /// Returns the include-from file path.
    pub(crate) fn include_from(&self) -> Option<&Path> {
        self.include_from.as_deref()
    }

    /// Returns the direct filter rules.
    #[allow(dead_code)]
    pub(crate) fn filter(&self) -> &[String] {
        &self.filter
    }

    /// Returns the direct exclude rules.
    #[allow(dead_code)]
    pub(crate) fn exclude(&self) -> &[String] {
        &self.exclude
    }

    /// Returns the direct include rules.
    #[allow(dead_code)]
    pub(crate) fn include(&self) -> &[String] {
        &self.include
    }

    /// Returns whether O_NOATIME is enabled.
    pub(crate) fn open_noatime(&self) -> bool {
        self.open_noatime
    }
}
