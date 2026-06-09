//! Common transfer setup shared by every receiver entry point.
//!
//! `setup_transfer` activates input multiplex, reads the filter list, receives
//! the (possibly incremental) file list, sanitizes paths, and builds the
//! `PipelineSetup` used by `run_sync`, `run_pipelined`, and
//! `run_pipelined_incremental`. Filter-list wire parsing lives alongside it in
//! `parse_wire_filters_for_receiver`.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;

use logging::{debug_log, info_log};
use metadata::MetadataOptions;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};

use filters::{DirMergeConfig, FilterChain, FilterSet};

use crate::receiver::{PHASE1_CHECKSUM_LENGTH, PipelineSetup, ReceiverContext};
use crate::shared::ChecksumFactory;
use crate::transfer_state::TransferPhase;

impl ReceiverContext {
    /// Common setup for all transfer modes.
    ///
    /// Activates input multiplex, reads filter list if needed, receives the file
    /// list (including INC_RECURSE extra segments), sanitizes paths, and builds
    /// the `PipelineSetup` with checksum and metadata configuration.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1342-1343` - client receiver activates multiplex at protocol >= 23
    /// - `main.c:1167-1168` - server receiver activates multiplex at protocol >= 30
    pub(in crate::receiver) fn setup_transfer<R: Read>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
    ) -> io::Result<(crate::reader::ServerReader<R>, usize, PipelineSetup)> {
        // upstream: generator.c:2260-2261 - emitted at the top of generate_files,
        // just before the per-segment dispatch loop. The receiver-side transfer
        // setup is the closest analog (every `run*` entry point routes through
        // setup_transfer).
        debug_log!(Genr, 1, "generator starting pid={}", std::process::id());

        // Parallel receive-side delta apply is unconditionally compiled (PFF-7).
        debug_log!(Recv, 1, "parallel receive-delta path active");

        let mut reader = if self.should_activate_input_multiplex() {
            reader.activate_multiplex().map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to activate INPUT multiplex: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?
        } else {
            reader
        };

        if self.should_read_filter_list() {
            let wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to read filter list: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?;

            // upstream: clientserver.c:rsync_module() - daemon_filter_list is applied
            // on top of client filters. Daemon rules take precedence (prepended).
            let daemon_rules = &self.config.daemon_filter_rules;
            let combined = if daemon_rules.is_empty() {
                wire_rules
            } else if wire_rules.is_empty() {
                daemon_rules.clone()
            } else {
                let mut combined = daemon_rules.clone();
                combined.extend(wire_rules);
                combined
            };

            // Build a FilterChain from the combined rules for deletion filtering.
            // upstream: generator.c:delete_in_dir() - is_excluded() before deletion
            if !combined.is_empty() {
                let (filter_set, merge_configs) = parse_wire_filters_for_receiver(&combined)
                    .map_err(|e| {
                        io::Error::new(
                            e.kind(),
                            format!(
                                "filter error: {e} {}{}",
                                crate::role_trailer::error_location!(),
                                crate::role_trailer::receiver()
                            ),
                        )
                    })?;
                let mut chain = FilterChain::new(filter_set);
                for config in merge_configs {
                    chain.add_merge_config(config);
                }
                self.filter_chain = chain;
            }
        }

        // FSM: filter list reading is complete. Advance to FileListTransfer.
        self.pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .map_err(crate::fsm_error)?;

        if self.config.flags.verbose && self.config.connection.client_mode {
            info_log!(Flist, 1, "receiving incremental file list");
        }

        let file_count = self.receive_file_list(&mut reader)?;

        let extra_count = self.receive_extra_file_lists(&mut reader)?;
        let file_count = file_count + extra_count;

        let removed = self.sanitize_file_list();
        let file_count = file_count - removed;

        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = PHASE1_CHECKSUM_LENGTH;

        let metadata_opts = MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            .preserve_times(self.config.flags.times)
            .preserve_atimes(self.config.flags.atimes)
            .preserve_crtimes(self.config.flags.crtimes)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids)
            // upstream: clientserver.c:1106-1107 - `fake super = yes` on the
            // daemon module forces fake-super metadata storage on the receiver
            // (ownership and special-file metadata go to user.rsync.%stat
            // xattrs instead of being applied to inodes).
            .fake_super(self.config.fake_super);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        let acl_cache = if self.config.flags.acls {
            self.flist_reader_cache
                .as_ref()
                .map(|r| Arc::new(r.acl_cache().clone()))
        } else {
            None
        };

        // SEC-1.e: open the destination root as a sandboxed dirfd carrier.
        // The carrier rides through every per-entry operation so the
        // SEC-1.f-j cutover sites can replace path-based syscalls with
        // their `*at` siblings without re-walking the path through the
        // kernel. This PR only threads the carrier; no syscalls are
        // migrated, so a failed open is non-fatal (a brand-new
        // destination root may not exist yet, and the existing
        // path-based fall-backs cover that case today).
        //
        // Daemon receivers without chroot tighten this: a leaf-symlink at
        // the destination is the chdir-symlink-race attack window, so we
        // refuse the transfer outright when DirSandbox open fails with
        // ELOOP/ENOTDIR instead of falling through to path-based syscalls
        // that would follow the symlink. ENOENT (first-run push that has
        // not created the directory yet) and EACCES (real permission
        // problems) keep the existing soft-fail behaviour.
        // upstream: clientserver.c:1018 - `use_secure_symlinks = am_daemon
        // && !am_chrooted` gates the do_*_at wrappers in syscall.c that
        // implement the same refusal.
        #[cfg(unix)]
        let sandbox = {
            let strict = self.config.connection.is_daemon_connection;
            open_sandbox_for_dest_strict(&dest_dir, strict)?
        };

        // FSM: file list received and sanitized. Advance to DeltaTransfer.
        self.pipeline
            .advance_to(TransferPhase::DeltaTransfer)
            .map_err(crate::fsm_error)?;

        Ok((
            reader,
            file_count,
            PipelineSetup {
                dest_dir,
                metadata_opts,
                checksum_length,
                checksum_algorithm,
                acl_cache,
                #[cfg(unix)]
                sandbox,
            },
        ))
    }
}

/// Open the destination root as a [`fast_io::DirSandbox`] carrier.
///
/// Returns `Some(Arc<DirSandbox>)` when the path exists and resolves to
/// a non-symlink directory the receiver can open. Returns `None` for any
/// other outcome (path does not exist yet, path is a symlink, EACCES,
/// etc.) so the receiver can keep running on the existing path-based
/// fall-backs while the SEC-1.f-j cutover lands site by site.
///
/// Failures are logged at `Debug` level only; they are expected on
/// first-run transfers where the destination is created later in
/// `ensure_relative_parents` / `create_directories`.
#[cfg(unix)]
#[allow(dead_code)] // kept for tests; the strict variant is the active call site.
fn open_sandbox_for_dest(dest_dir: &std::path::Path) -> Option<Arc<fast_io::DirSandbox>> {
    match fast_io::DirSandbox::open_root(dest_dir) {
        Ok(sandbox) => Some(Arc::new(sandbox)),
        Err(err) => {
            logging::debug_log!(
                Recv,
                2,
                "DirSandbox::open_root({}) failed: {err}; falling back to path-based syscalls",
                dest_dir.display()
            );
            None
        }
    }
}

/// Open the destination root as a [`fast_io::DirSandbox`] carrier and, when
/// `strict` is set, propagate symlink-class refusals as a transfer error.
///
/// When `strict` is `false` this is identical to [`open_sandbox_for_dest`]:
/// every failure falls back to path-based syscalls.
///
/// When `strict` is `true` the failure mode splits by errno:
/// - `ELOOP` / `ENOTDIR`: the destination resolves through a symlink, which
///   is the chdir-symlink-race attack window. Convert to `io::Error` so the
///   transfer fails before any data lands on disk and no path-relative
///   syscall ever resolves through the attacker-planted symlink.
/// - `ENOENT`: the destination does not exist yet (first-run push). Return
///   `Ok(None)` so the receiver creates it through the existing
///   `ensure_relative_parents` / `create_directories` path.
/// - Any other error: keep the soft-fall-back so legitimate permission or
///   I/O problems surface at a more specific call site downstream.
///
/// # Upstream Reference
///
/// - `clientserver.c:1018` - `use_secure_symlinks = am_daemon &&
///   !am_chrooted` gates the do_*_at wrappers in `syscall.c`.
/// - `util1.c:1175-1216` - `change_dir()`'s
///   `secure_relative_open()` + `fchdir()` branch refuses the symlink at the
///   same level the chdir-symlink-race POC plants it.
#[cfg(unix)]
fn open_sandbox_for_dest_strict(
    dest_dir: &std::path::Path,
    strict: bool,
) -> io::Result<Option<Arc<fast_io::DirSandbox>>> {
    match fast_io::DirSandbox::open_root(dest_dir) {
        Ok(sandbox) => Ok(Some(Arc::new(sandbox))),
        Err(err) => {
            let code = err.raw_os_error();
            let is_symlink_refusal = matches!(
                code,
                Some(libc::ELOOP) | Some(libc::ENOTDIR) | Some(libc::EXDEV)
            );
            if strict && is_symlink_refusal {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "refusing to open destination '{}' via a symlink: {err} \
                         (would expose the chdir-symlink-race attack window)",
                        dest_dir.display()
                    ),
                ));
            }
            logging::debug_log!(
                Recv,
                2,
                "DirSandbox::open_root({}) failed: {err}; falling back to path-based syscalls",
                dest_dir.display()
            );
            Ok(None)
        }
    }
}

/// Parses wire-format filter rules into a `FilterSet` and `DirMergeConfig` list for the receiver.
///
/// Separates DirMerge rules (for per-directory merge file scanning) from regular
/// filter rules. The returned `FilterSet` contains compiled include/exclude/protect/risk
/// rules. The `DirMergeConfig` list configures per-directory merge file scanning
/// used during deletion filtering.
///
/// # Upstream Reference
///
/// - `exclude.c:recv_filter_list()` - receiver-side filter list reception
/// - `generator.c:delete_in_dir()` - deletion pass uses filter evaluation
fn parse_wire_filters_for_receiver(
    wire_rules: &[FilterRuleWireFormat],
) -> io::Result<(FilterSet, Vec<DirMergeConfig>)> {
    use ::filters::FilterRule;

    let mut rules = Vec::with_capacity(wire_rules.len());
    let mut merge_configs = Vec::new();

    for wire_rule in wire_rules {
        let mut rule = match wire_rule.rule_type {
            RuleType::Include => FilterRule::include(&wire_rule.pattern),
            RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
            RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
            RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
            RuleType::Clear => {
                rules.push(
                    FilterRule::clear().with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                );
                continue;
            }
            RuleType::DirMerge => {
                let mut config = DirMergeConfig::new(&wire_rule.pattern);
                if wire_rule.no_inherit {
                    config = config.with_inherit(false);
                }
                if wire_rule.exclude_from_merge {
                    config = config.with_exclude_self(true);
                }
                if wire_rule.sender_side {
                    config = config.with_sender_only(true);
                }
                if wire_rule.receiver_side {
                    config = config.with_receiver_only(true);
                }
                if wire_rule.perishable {
                    config = config.with_perishable(true);
                }
                merge_configs.push(config);
                continue;
            }
            RuleType::Merge => continue,
        };

        if wire_rule.sender_side || wire_rule.receiver_side {
            rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
        }
        if wire_rule.perishable {
            rule = rule.with_perishable(true);
        }
        if wire_rule.xattr_only {
            rule = rule.with_xattr_only(true);
        }
        if wire_rule.negate {
            rule = rule.with_negate(true);
        }
        if wire_rule.anchored {
            rule = rule.anchor_to_root();
        }

        rules.push(rule);
    }

    let filter_set = FilterSet::from_rules(rules)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("filter error: {e}")))?;

    Ok((filter_set, merge_configs))
}

// upstream: clientserver.c:1018 - use_secure_symlinks gating that the
// chdir-symlink-race fix mirrors. Tests below verify the strict daemon
// branch refuses a leaf-symlink at the destination while the legacy
// non-daemon branch preserves the existing soft-fail behaviour.
#[cfg(all(test, unix))]
mod symlink_race_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().expect("tempdir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
        (dir, canon)
    }

    #[test]
    fn strict_mode_refuses_symlink_destination() {
        let (_keep, root) = canonical_tempdir();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).expect("create outside dir");
        let subdir = root.join("subdir");
        symlink(&outside, &subdir).expect("symlink subdir -> outside");

        let err = open_sandbox_for_dest_strict(&subdir, true)
            .expect_err("daemon receiver must refuse a symlink destination");
        // ELOOP on Linux + openat2; ENOTDIR on macOS/BSD where O_DIRECTORY
        // is evaluated before O_NOFOLLOW; both prove the symlink was
        // refused.
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "expected ELOOP or ENOTDIR for symlink dest, got: {err}"
        );
    }

    #[test]
    fn non_strict_mode_falls_back_for_symlink_destination() {
        let (_keep, root) = canonical_tempdir();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).expect("create outside dir");
        let subdir = root.join("subdir");
        symlink(&outside, &subdir).expect("symlink subdir -> outside");

        let result = open_sandbox_for_dest_strict(&subdir, false)
            .expect("non-daemon receiver keeps soft-fail behaviour");
        // The sandbox open failed, but the receiver still gets None and
        // falls through to the path-based syscall path (existing
        // behaviour before the chdir-symlink-race fix).
        assert!(result.is_none());
    }

    #[test]
    fn strict_mode_accepts_real_directory_destination() {
        let (_keep, root) = canonical_tempdir();
        let real = root.join("realdir");
        std::fs::create_dir(&real).expect("create real dir");

        let result = open_sandbox_for_dest_strict(&real, true)
            .expect("real directory must open under strict mode");
        assert!(
            result.is_some(),
            "strict mode must hand back a sandbox when the dest is a real dir"
        );
    }

    #[test]
    fn strict_mode_soft_fails_when_destination_is_missing() {
        let (_keep, root) = canonical_tempdir();
        let missing = root.join("not-yet-created");

        let result = open_sandbox_for_dest_strict(&missing, true)
            .expect("ENOENT must be a soft failure - first-run push will mkdir later");
        assert!(result.is_none());
    }
}
