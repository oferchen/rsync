//! Computes the implied-include source arguments recorded for a pull so the
//! receiver can validate the incoming file list (CVE-2022-29154).
//!
//! Mirrors upstream's `add_implied_include()` call sites and the
//! `trust_sender_args` conditions that disable the mechanism.
//!
//! # Upstream Reference
//!
//! - `main.c:1524-1549` - each requested remote source arg is recorded, but
//!   only when `filesfrom_fd < 0` (no `--files-from`).
//! - `io.c:427,464` - with a local `--files-from`, each forwarded list entry is
//!   recorded instead (as the bytes are streamed to the remote sender).
//! - `options.c:2510-2513` - `trust_sender_args` (which makes
//!   `add_implied_include()` a no-op) is set for `--old-args`/`RSYNC_OLD_ARGS`
//!   (`old_style_args`) and for a remote `--files-from` (`filesfrom_host`).

use crate::client::config::ClientConfig;

/// Returns the implied-include source args for a pull transfer.
///
/// `source_paths` are the host-stripped remote source operands. `files_from_data`
/// is the staged local `--files-from` byte stream (NUL-separated entries), or
/// `None` when the list is not staged locally (no `--files-from`, or a remote
/// `--files-from` the sender reads directly).
///
/// An empty result leaves the receiver-side check inactive, matching upstream's
/// empty `implied_filter_list`.
pub(crate) fn implied_source_args_for_pull(
    config: &ClientConfig,
    source_paths: &[String],
    files_from_data: Option<&[u8]>,
) -> Vec<String> {
    // upstream: options.c:2513 - old_style_args sets trust_sender_args, so
    // add_implied_include() returns early and the implied list stays empty.
    if config.old_args() == Some(true) {
        return Vec::new();
    }

    if config.files_from().is_active() {
        // upstream: main.c:1524 - the source arg is NOT recorded when
        // --files-from is active; io.c:427/464 records each forwarded entry.
        return match files_from_data {
            Some(bytes) => files_from_entries(bytes),
            // Remote --files-from (filesfrom_host != NULL) sets trust_sender_args
            // (options.c:2513): no local entries, mechanism disabled.
            None => Vec::new(),
        };
    }

    source_paths.to_vec()
}

/// Splits the staged `--files-from` wire bytes (NUL-separated, double-NUL
/// terminated) into individual entries, preserving each entry's `/./` pivots
/// and trailing slashes so [`filters::ImpliedIncludes`] reproduces upstream's
/// per-entry `add_implied_include()` processing.
fn files_from_entries(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{files_from_entries, implied_source_args_for_pull};
    use crate::client::config::{ClientConfig, FilesFromSource};

    fn config_with(old_args: Option<bool>, files_from: FilesFromSource) -> ClientConfig {
        ClientConfig::builder()
            .old_args(old_args)
            .files_from(files_from)
            .build()
    }

    #[test]
    fn plain_pull_records_the_source_paths() {
        let config = config_with(None, FilesFromSource::None);
        let sources = vec!["dir".to_owned(), "other".to_owned()];
        assert_eq!(
            implied_source_args_for_pull(&config, &sources, None),
            sources
        );
    }

    #[test]
    fn old_args_disables_the_mechanism() {
        // upstream: options.c:2513 - old_style_args sets trust_sender_args.
        let config = config_with(Some(true), FilesFromSource::None);
        assert!(implied_source_args_for_pull(&config, &["dir".to_owned()], None).is_empty());
    }

    #[test]
    fn local_files_from_folds_entries_and_drops_source_arg() {
        // upstream: main.c:1524 skips the source arg; io.c:427/464 records each
        // forwarded entry instead.
        let config = config_with(None, FilesFromSource::Stdin);
        let bytes = b"from/./\0from/./dir/subdir\0\0";
        assert_eq!(
            implied_source_args_for_pull(&config, &["ignored".to_owned()], Some(bytes)),
            vec!["from/./".to_owned(), "from/./dir/subdir".to_owned()]
        );
    }

    #[test]
    fn remote_files_from_disables_the_mechanism() {
        // upstream: options.c:2513 - filesfrom_host != NULL sets trust_sender_args;
        // no bytes are staged locally.
        let config = config_with(None, FilesFromSource::RemoteFile("list".to_owned()));
        assert!(implied_source_args_for_pull(&config, &["dir".to_owned()], None).is_empty());
    }

    #[test]
    fn parses_nul_separated_entries_preserving_slashes() {
        let bytes = b"from/./\0from/./dir/subdir\0from/./dir/subsubdir2/\0\0";
        assert_eq!(
            files_from_entries(bytes),
            vec![
                "from/./".to_owned(),
                "from/./dir/subdir".to_owned(),
                "from/./dir/subsubdir2/".to_owned(),
            ]
        );
    }

    #[test]
    fn empty_stream_yields_no_entries() {
        assert!(files_from_entries(b"\0").is_empty());
        assert!(files_from_entries(b"").is_empty());
    }
}
