use super::*;

impl ClientConfig {
    /// Returns the raw transfer arguments provided by the caller.
    #[must_use]
    pub fn transfer_args(&self) -> &[OsString] {
        &self.transfer_args
    }

    /// Returns the ordered reference directories supplied via `--compare-dest`,
    /// `--copy-dest`, or `--link-dest`.
    #[must_use]
    #[doc(alias = "--compare-dest")]
    #[doc(alias = "--copy-dest")]
    #[doc(alias = "--link-dest")]
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Reports whether transfers should be listed without mutating the destination.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(&self) -> bool {
        self.list_only
    }

    /// Reports whether the user passed `--list-only` explicitly.
    ///
    /// Mirrors upstream `list_only > 1`: only the explicit flag is forwarded to
    /// the remote as `--list-only` (`options.c:2747`); the implicit single-source
    /// listing (`list_only == 1`) is not.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only_arg(&self) -> bool {
        self.list_only_arg
    }

    /// Reports whether `-q` / `--quiet` was passed (upstream `quiet`).
    #[must_use]
    #[doc(alias = "--quiet")]
    pub const fn quiet(&self) -> bool {
        self.quiet
    }

    /// Returns the tri-state `--msgs2stderr` setting (upstream `msgs2stderr`).
    ///
    /// `None` is the default (upstream value 2); `Some(true)` is `--msgs2stderr`
    /// (value 1); `Some(false)` is `--no-msgs2stderr` (value 0).
    #[must_use]
    #[doc(alias = "--msgs2stderr")]
    pub const fn msgs2stderr(&self) -> Option<bool> {
        self.msgs2stderr
    }

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub const fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }

    /// Returns the configured batch mode settings, if any.
    #[doc(alias = "--write-batch")]
    #[doc(alias = "--only-write-batch")]
    #[doc(alias = "--read-batch")]
    pub const fn batch_config(&self) -> Option<&engine::batch::BatchConfig> {
        self.batch_config.as_ref()
    }

    /// Returns `true` when the local client should emit the sender-side `<`
    /// itemize direction arrow (push over a remote shell or daemon).
    ///
    /// The oc-rsync formatter at `itemize.rs::format_itemized_changes` picks
    /// `<` when this flag is set and `>` otherwise. It does not model
    /// upstream's separate `local_server` branch, so this helper must only
    /// report `true` when the destination is remote AND every source is
    /// local - the exact case upstream renders as `<`.
    ///
    /// upstream: log.c:701-704 - `!local_server && *op == 's' ? '<' : '>'`
    #[must_use]
    pub fn is_local_sender(&self) -> bool {
        use crate::client::remote::operand_is_remote;

        if self.transfer_args.len() < 2 {
            return false;
        }

        let (sources, destination) = self.transfer_args.split_at(self.transfer_args.len() - 1);
        let any_source_remote = sources.iter().any(|s| operand_is_remote(s));
        let dest_remote = destination
            .first()
            .map(|d| operand_is_remote(d))
            .unwrap_or(false);

        // Push (remote dest, no remote sources): local is the SSH/daemon
        //   sender -> upstream emits `<`, return true.
        // Pull (any remote source): local is the receiver -> upstream emits
        //   `>`, return false.
        // Local copy (no remote operands): upstream's `local_server` branch
        //   emits `>` -> return false so oc-rsync's formatter matches.
        // Proxy (both remote): no per-file itemize lines render locally;
        //   return false so the arrow defaults to `>` for the unreachable
        //   case.
        dest_remote && !any_source_remote
    }

    /// Whether this transfer is a pull: at least one source operand is remote,
    /// so the local process is the receiver rather than the sender.
    ///
    /// A push (remote destination, local sources) and a local copy both send
    /// the file list from the local process, so neither is a pull.
    ///
    /// upstream: `flist.c:2251` prints "sending incremental file list" only on
    /// the sender (`!am_server`); the client is `am_sender` for a push and a
    /// local copy, and the receiver for a pull (which prints "receiving").
    #[must_use]
    pub fn is_pull(&self) -> bool {
        use crate::client::remote::operand_is_remote;

        if self.transfer_args.len() < 2 {
            return false;
        }

        let (sources, _destination) = self.transfer_args.split_at(self.transfer_args.len() - 1);
        sources.iter().any(|s| operand_is_remote(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    #[test]
    fn transfer_args_default_is_empty() {
        let config = default_config();
        assert!(config.transfer_args().is_empty());
    }

    #[test]
    fn reference_directories_default_is_empty() {
        let config = default_config();
        assert!(config.reference_directories().is_empty());
    }

    #[test]
    fn list_only_default_is_false() {
        let config = default_config();
        assert!(!config.list_only());
    }

    #[test]
    fn has_transfer_request_default_is_false() {
        let config = default_config();
        assert!(!config.has_transfer_request());
    }

    #[test]
    fn batch_config_default_is_none() {
        let config = default_config();
        assert!(config.batch_config().is_none());
    }

    #[test]
    fn is_local_sender_local_copy_reports_receiver() {
        // Pure-local copy: `oc-rsync src/ dst/`. Upstream's `log.c:704` falls
        // into the `local_server` branch which emits `>`. The oc-rsync formatter
        // does not model `local_server`, so report `false` here to keep the
        // arrow defaulting to `>` and stay byte-identical with upstream.
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src/"), OsString::from("dst/")])
            .build();
        assert!(!config.is_local_sender());
    }

    #[test]
    fn is_local_sender_push_reports_sender() {
        // Push: `oc-rsync src/ host:dst/`. Local client is the sender; upstream
        // log.c:704 emits `<` because !local_server && am_sender.
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src/"), OsString::from("host:dst/")])
            .build();
        assert!(config.is_local_sender());
    }

    #[test]
    fn is_local_sender_pull_reports_receiver() {
        // Pull: `oc-rsync host:src/ dst/`. Local client is the receiver; upstream
        // log.c:704 emits `>` because *op != 's'.
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("host:src/"), OsString::from("dst/")])
            .build();
        assert!(!config.is_local_sender());
    }

    #[test]
    fn is_local_sender_rsync_url_pull_reports_receiver() {
        // Daemon pull via rsync:// URL.
        let config = ClientConfig::builder()
            .transfer_args([
                OsString::from("rsync://host/mod/src/"),
                OsString::from("dst/"),
            ])
            .build();
        assert!(!config.is_local_sender());
    }

    #[test]
    fn is_local_sender_double_colon_push_reports_sender() {
        // Daemon push via `host::module` syntax.
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src/"), OsString::from("host::mod/dst/")])
            .build();
        assert!(config.is_local_sender());
    }

    #[test]
    fn is_local_sender_empty_args_reports_receiver() {
        // No transfer args (module-list mode etc.). Default to receiver so the
        // itemize formatter emits `>` for any callers that do render lines,
        // matching the upstream `local_server` polarity.
        let config = ClientConfig::default();
        assert!(!config.is_local_sender());
    }
}
