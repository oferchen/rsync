use super::*;

impl ClientConfigBuilder {
    /// Sets the verbosity level requested by the caller.
    #[must_use]
    #[doc(alias = "--verbose")]
    #[doc(alias = "-v")]
    pub const fn verbosity(mut self, verbosity: u8) -> Self {
        self.verbosity = verbosity;
        self
    }

    /// Enables or disables progress reporting for the transfer.
    #[must_use]
    #[doc(alias = "--progress")]
    #[doc(alias = "--no-progress")]
    pub const fn progress(mut self, progress: bool) -> Self {
        self.progress = progress;
        self
    }

    /// Enables or disables statistics reporting for the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub const fn stats(mut self, stats: bool) -> Self {
        self.stats = stats;
        self
    }

    /// Enables or disables human-readable output formatting.
    #[must_use]
    #[doc(alias = "--human-readable")]
    pub const fn human_readable(mut self, enabled: bool) -> Self {
        self.human_readable = enabled;
        self
    }

    /// Enables or disables itemize-changes output.
    ///
    /// When true, per-file change summaries are emitted. For remote transfers,
    /// this flag is forwarded to the server via the `.i` info flag.
    #[must_use]
    #[doc(alias = "--itemize-changes")]
    #[doc(alias = "-i")]
    pub const fn itemize_changes(mut self, enabled: bool) -> Self {
        self.itemize_changes = enabled;
        self
    }

    /// When true, itemize rows are emitted for unchanged entries too.
    ///
    /// Set when `-i` was given at least twice (`stdout_format_has_i > 1`) or
    /// `--info=name2` raised the NAME level. Mirrors upstream's `itemize()`
    /// emit gate at generator.c:575-576.
    #[must_use]
    #[doc(alias = "-ii")]
    pub const fn itemize_unchanged(mut self, enabled: bool) -> Self {
        self.itemize_unchanged = enabled;
        self
    }

    /// Suppresses daemon MOTD (message of the day) output.
    #[must_use]
    #[doc(alias = "--no-motd")]
    pub const fn no_motd(mut self, no_motd: bool) -> Self {
        self.no_motd = no_motd;
        self
    }

    /// Sets a pre-loaded password override for daemon authentication.
    ///
    /// When `Some`, this password takes precedence over the `RSYNC_PASSWORD`
    /// environment variable during the daemon handshake. Typically populated
    /// from `--password-command` or `--password-file`.
    #[must_use]
    pub fn password_override(mut self, password: Option<Vec<u8>>) -> Self {
        self.password_override = password;
        self
    }

    /// Configures daemon parameter overrides sent during the daemon handshake.
    ///
    /// Each entry should be a `key=value` string that overrides a module-level
    /// configuration directive on the daemon. Mirrors upstream rsync's
    /// `--dparam` / `-M` option (clientserver.c).
    #[must_use]
    #[doc(alias = "--dparam")]
    #[doc(alias = "-M")]
    pub fn daemon_params(mut self, params: Vec<String>) -> Self {
        self.daemon_params = params;
        self
    }
}
