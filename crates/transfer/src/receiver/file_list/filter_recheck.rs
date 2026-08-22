//! Receiver-side re-filtering of received file-list names.
//!
//! Defense-in-depth: after the sender transmits the file list, the receiver
//! re-runs each received name through its own filter chain and aborts the
//! transfer if a name the receiver's rules exclude arrives anyway. A
//! well-behaved sender never sends such a name (it applies the same rules when
//! building the list), so its presence means a buggy or malicious sender is
//! trying to make the receiver materialize a path it excluded.
//!
//! # Upstream Reference
//!
//! - `flist.c:1019-1030` `recv_file_entry()` - for every received name (except
//!   the transfer root `.`), `check_server_filter(&filter_list, ...)` is run;
//!   a match on an exclude rule (`< 0`) triggers
//!   `rprintf(FERROR, "ERROR: rejecting excluded file-list name: %s\n", ...)`
//!   followed by `exit_cleanup(RERR_UNSUPPORTED)`.
//! - `exclude.c:1027-1034` `check_server_filter()` - evaluates the list with
//!   `cur_elide_value = LOCAL_RULE`, i.e. the sender-side view of the rules
//!   (receiver-side-only rules are elided), matching the sender's own
//!   include/exclude decision.
//! - `exclude.c:1182` - a per-directory merge rule (`:`) sets
//!   `trust_sender_filter = 1`; the receiver then trusts the sender's filtering
//!   and skips the re-check entirely, because it cannot reliably reproduce a
//!   per-directory merge decision without the sender's directory context.

use std::io;
use std::path::Path;

use filters::{FilterSet, ImpliedIncludeOptions, ImpliedIncludes};
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use crate::receiver::transfer::parse_wire_filters_for_receiver;

impl ReceiverContext {
    /// Re-checks received file-list names against the receiver's own filter
    /// rules, aborting the transfer if the sender sent a name the receiver
    /// excludes.
    ///
    /// Runs after [`sanitize_file_list`](Self::sanitize_file_list) so the paths
    /// examined here are already cleaned (no absolute or `..` components),
    /// mirroring upstream `recv_file_entry()` which sanitizes each name before
    /// the filter re-check.
    ///
    /// Returns [`io::ErrorKind::Unsupported`] on the first excluded name so the
    /// error maps to `RERR_UNSUPPORTED` (exit code 4), matching upstream's
    /// `exit_cleanup(RERR_UNSUPPORTED)`.
    ///
    /// The re-check is skipped when:
    /// - `trust_sender` is set (upstream `trust_sender_filter`: local transfer,
    ///   `--trust-sender`), or
    /// - the effective filter set uses per-directory merge files (upstream sets
    ///   `trust_sender_filter = 1` for any `:` rule), or
    /// - there are no receiver-owned filter rules to check.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1049-1052` `recv_file_entry()`
    pub(in crate::receiver) fn recheck_received_filter(&self) -> io::Result<()> {
        self.recheck_received_filter_entries(&self.file_list)
    }

    /// Range-scoped variant of
    /// [`recheck_received_filter`](Self::recheck_received_filter) that re-checks
    /// only `entries` (an INC_RECURSE sub-list at `self.file_list[flat_start..]`),
    /// so sub-list entries received on a pull get the same defense-in-depth as
    /// the level-1 list. upstream: flist.c:1022 `recv_file_entry()` runs the
    /// server-filter check for every received entry, including sub-lists.
    pub(in crate::receiver) fn recheck_received_filter_entries(
        &self,
        entries: &[FileEntry],
    ) -> io::Result<()> {
        // upstream: flist.c:1021 - `!trust_sender_filter` guards the whole
        // re-check (options.c:2512/main.c:1496 set it for local/--trust-sender).
        if self.config.trust_sender {
            return Ok(());
        }

        // Resolve the receiver's own filter set. A server-receiver compiles the
        // rules it read off the wire into `filter_chain`; a local-client pull
        // never reads a wire filter list and keeps its CLI rules in
        // `config.connection.filter_rules` instead.
        let owned_set;
        let filter_set: &FilterSet = if !self.filter_chain.is_empty() {
            // upstream: exclude.c:1182 - a `:` (per-dir merge) rule sets
            // trust_sender_filter, so the receiver defers to the sender.
            if self.filter_chain.has_per_dir_merge() {
                return Ok(());
            }
            self.filter_chain.global()
        } else if self.config.connection.client_mode
            && !self.config.connection.filter_rules.is_empty()
        {
            let (set, merge_configs) =
                parse_wire_filters_for_receiver(&self.config.connection.filter_rules)?;
            if !merge_configs.is_empty() {
                return Ok(());
            }
            owned_set = set;
            &owned_set
        } else {
            return Ok(());
        };

        for entry in entries {
            let path = entry.path().as_path();
            // upstream: flist.c:1019 - the transfer root (`.` / `/.`) is never
            // re-checked. Cleared entries (empty name, mode 0) are no-ops.
            if path.as_os_str().is_empty() || path == Path::new(".") {
                continue;
            }

            // upstream: flist.c:1022 - `check_server_filter(...) < 0` means the
            // sender-side rules exclude this name. `allows_during_traversal` is
            // the sender's decision (DecisionContext::Transfer, receiver-only
            // rules elided) evaluated with upstream `exclude.c:rule_matches()`
            // semantics: no synthetic `pattern/**` descendant matchers. This is
            // load-bearing - `rule_matches()` (exclude.c:903, `!ex->u.slash_cnt`
            // basename strip) never matches a child against a slashless anchored
            // exclude like `- /bar`, so a name the sender legitimately sent
            // under an included parent (e.g. `bar/down` after `+ **/bar`) must
            // NOT be rejected here. `!allows_during_traversal` is exactly
            // upstream's `check_server_filter(...) < 0`.
            if !filter_set.allows_during_traversal(path, entry.is_dir()) {
                // upstream: flist.c:1023-1024 - rprintf(FERROR, ...) then
                // exit_cleanup(RERR_UNSUPPORTED).
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "ERROR: rejecting excluded file-list name: {}",
                        path.display()
                    ),
                ));
            }
        }

        Ok(())
    }

    /// Re-checks received file-list names against the implied-include set built
    /// from the client's requested source args, aborting the transfer if the
    /// sender sent a name that was never requested (CVE-2022-29154).
    ///
    /// A malicious sender can otherwise inject extra file-list entries that the
    /// client never asked for, causing the receiver to materialize paths
    /// outside the intended set. Upstream records each requested source arg as
    /// an implied include and rejects any received name not covered by it.
    ///
    /// Runs after [`recheck_received_filter`](Self::recheck_received_filter) so
    /// the ordering mirrors upstream `recv_file_entry()` (server-filter check
    /// then implied-include check). Returns [`io::ErrorKind::Unsupported`] on
    /// the first unrequested name so the error maps to `RERR_UNSUPPORTED`
    /// (exit code 4), matching upstream's `exit_cleanup(RERR_UNSUPPORTED)`.
    ///
    /// The check is skipped when:
    /// - `trust_sender` is set (upstream `trust_sender_args`: local, server,
    ///   `--trust-sender`, old-style, or `files-from-host` transfers, where
    ///   `add_implied_include()` returns early), or
    /// - no source args were recorded (a push/server-receiver never records
    ///   them, so the implied list is empty).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1026-1029` `recv_file_entry()` - `check_filter(&implied_filter_list,
    ///   ...) <= 0` triggers `rprintf(FERROR, "ERROR: rejecting unrequested
    ///   file-list name: %s\n", ...)` then `exit_cleanup(RERR_UNSUPPORTED)`.
    /// - `exclude.c:379` `add_implied_include()` - rule construction.
    /// - `options.c:2510-2513` - `trust_sender_args` disables the mechanism.
    pub(in crate::receiver) fn recheck_received_implied_includes(&self) -> io::Result<()> {
        self.recheck_received_implied_includes_entries(&self.file_list)
    }

    /// Range-scoped variant of
    /// [`recheck_received_implied_includes`](Self::recheck_received_implied_includes)
    /// that validates only `entries` (an INC_RECURSE sub-list at
    /// `self.file_list[flat_start..]`) so sub-list entries get the same
    /// CVE-2022-29154 defense as the level-1 list. upstream: flist.c:1026
    /// `recv_file_entry()` runs the implied-include check for every received
    /// entry, including sub-lists.
    pub(in crate::receiver) fn recheck_received_implied_includes_entries(
        &self,
        entries: &[FileEntry],
    ) -> io::Result<()> {
        let Some(implied) = self.build_implied_includes()? else {
            return Ok(());
        };

        for entry in entries {
            let path = entry.path().as_path();
            // upstream: flist.c:1019 - the transfer root (`.` / `/.`) is exempt.
            if path.as_os_str().is_empty() || path == Path::new(".") {
                continue;
            }

            // upstream: flist.c:1026 - check_filter(&implied_filter_list, ...)
            // <= 0 means no include rule matched, i.e. the name was never
            // requested. `covers` reproduces `check_filter(...) > 0` with
            // `rule_matches()` (check_descendants = false) semantics.
            if !implied.covers(path, entry.is_dir()) {
                // upstream: flist.c:1027-1028 - rprintf(FERROR, ...) then
                // exit_cleanup(RERR_UNSUPPORTED).
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "ERROR: rejecting unrequested file-list name: {}",
                        path.display()
                    ),
                ));
            }
        }

        Ok(())
    }

    /// Builds the implied-include set from the client's requested source args,
    /// or `None` when the mechanism is inactive.
    ///
    /// Shared by the unrequested-name refusal and the implied-parent downgrade,
    /// which upstream drives from the one `implied_filter_list` global.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2510` / `exclude.c:385` - `trust_sender_args` makes
    ///   `add_implied_include()` a no-op, leaving `implied_filter_list` empty.
    /// - `exclude.c:403-567` - `relative_paths`, `recurse`, `xfer_dirs` and the
    ///   daemon-module flag shape the implied rules. The module-name strip
    ///   applies only to a daemon source arg (`main.c:1549` passes
    ///   `skip_daemon_module=daemon_connection`); local `--files-from` entries
    ///   are already module-relative, so upstream records them with
    ///   `skip_daemon_module=0` (`io.c:427,464`). The strip decision is read
    ///   from the stable flag recorded when the args were built:
    ///   `files_from_data` is consumed while forwarding the list and would
    ///   misreport here.
    fn build_implied_includes(&self) -> io::Result<Option<ImpliedIncludes>> {
        if self.config.trust_sender {
            return Ok(None);
        }

        let sources = &self.config.connection.implied_source_args;
        if sources.is_empty() {
            return Ok(None);
        }

        let opts = ImpliedIncludeOptions {
            relative: self.config.flags.relative,
            recurse: self.config.flags.recursive,
            dirs: self.config.flags.dirs,
            skip_daemon_module: self.config.connection.implied_skip_daemon_module,
        };
        let implied = ImpliedIncludes::from_args(opts, sources)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        Ok((!implied.is_empty()).then_some(implied))
    }

    /// Forces every implied-parent directory entry back to the honest
    /// non-content encoding, whatever xflags byte the sender sent.
    ///
    /// A malicious sender can omit `XMIT_NO_CONTENT_DIR` from an implied parent
    /// (whose honest encoding is `XMIT_TOP_DIR | XMIT_NO_CONTENT_DIR`) to make
    /// the receiver treat it as a content dir. The deletion pass would then run
    /// on that directory and sweep pre-existing siblings the client never asked
    /// to touch - a `--delete` scope expansion chosen by the peer.
    ///
    /// `implied_source_args` is receiver-owned state, so this is deliberately
    /// **not** gated on the sender-trusting escape hatches that suppress the
    /// per-directory filter re-check: a per-dir merge rule must not be able to
    /// downgrade the defense.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1230-1252` `recv_file_entry()` - re-asserts
    ///   `XMIT_NO_CONTENT_DIR | XMIT_TOP_DIR` before flag mapping, so the entry
    ///   lands in `FLAG_IMPLIED_DIR` rather than `FLAG_CONTENT_DIR`.
    /// - `exclude.c:1144` `is_implied_parent_dir()` - the parent-only test.
    pub(in crate::receiver) fn downgrade_implied_parent_dirs(&mut self) -> io::Result<()> {
        self.downgrade_implied_parent_dirs_from(0)
    }

    /// Range-scoped variant of
    /// [`downgrade_implied_parent_dirs`](Self::downgrade_implied_parent_dirs)
    /// covering only `self.file_list[flat_start..]`, so INC_RECURSE sub-list
    /// entries get the same defense. upstream: `flist.c:1230` runs per received
    /// entry, sub-lists included.
    pub(in crate::receiver) fn downgrade_implied_parent_dirs_from(
        &mut self,
        flat_start: usize,
    ) -> io::Result<()> {
        let Some(implied) = self.build_implied_includes()? else {
            return Ok(());
        };

        for entry in &mut self.file_list[flat_start..] {
            if !entry.is_dir() || !entry.content_dir() {
                continue;
            }
            if implied.is_parent_only_dir(entry.path().as_path()) {
                // upstream: flist.c:1249 - both flags, so the entry maps to
                // FLAG_IMPLIED_DIR (oc: top_dir without content_dir).
                entry.set_content_dir(false);
                entry.set_top_dir(true);
            }
        }

        Ok(())
    }
}
