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

    /// Returns `true` when the local client is the sender (push transfer).
    ///
    /// Inspects `transfer_args`: when no source operand is remote the local
    /// client acts as the sender. Pure-local transfers (no remote operands at
    /// all) also report the local client as sender by upstream convention -
    /// they hit the `local_server` branch in `log.c:704` which independently
    /// selects `>`, so the polarity is harmless there.
    ///
    /// Used by the output formatter to pick the itemize direction arrow:
    /// upstream emits `<` for sender-via-SSH, `>` otherwise.
    ///
    /// upstream: log.c:701-704 - `!local_server && *op == 's' ? '<' : '>'`
    #[must_use]
    pub fn is_local_sender(&self) -> bool {
        use crate::client::remote::operand_is_remote;

        if self.transfer_args.len() < 2 {
            return true;
        }

        let (sources, _destination) = self.transfer_args.split_at(self.transfer_args.len() - 1);
        let any_source_remote = sources.iter().any(|s| operand_is_remote(s));

        // Pull (any remote source): local is the receiver -> false.
        // Push (remote dest, no remote sources): local is the sender -> true.
        // Local copy (no remote operands): treated as sender by upstream
        //   convention - itemize hits `local_server` and emits `>` regardless,
        //   so the polarity is harmless.
        // Proxy (both remote): no per-file itemize lines render locally; the
        //   value is unobservable. Reporting `false` keeps the arrow defaulting
        //   to `>` for the unreachable case.
        !any_source_remote
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
    fn is_local_sender_local_copy_reports_sender() {
        // Pure-local copy: `oc-rsync src/ dst/`. Upstream's `log.c:704` falls
        // into the `local_server` branch which emits `>` regardless of am_sender,
        // so the polarity is harmless. Lock the upstream convention here.
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src/"), OsString::from("dst/")])
            .build();
        assert!(config.is_local_sender());
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
    fn is_local_sender_empty_args_reports_sender() {
        // No transfer args (module-list mode etc.). Default to sender so the
        // itemize formatter doesn't flip arrows for callers that never render
        // per-file lines.
        let config = ClientConfig::default();
        assert!(config.is_local_sender());
    }
}
