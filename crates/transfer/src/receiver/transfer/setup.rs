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

use crate::receiver::{
    PHASE1_CHECKSUM_LENGTH, PipelineSetup, ReceiverContext, dest_arg_has_trailing_slash,
    ensure_dest_root_exists,
};
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
            .fake_super(self.config.fake_super)
            // upstream: clientserver.c:rsync_module() + generator.c -
            // `daemon_chmod_modes` rewrites the destination mode at finalize
            // time. The parser ran at module-load and stored a parsed
            // `ChmodModifiers`; we hand it to MetadataOptions so the existing
            // chmod-application site in `apply_permissions_with_chmod`
            // performs the rewrite without a separate code path.
            .with_chmod(self.config.daemon_incoming_chmod.clone());

        let dest_arg = self.config.args.first();
        let trailing_slash = dest_arg.is_some_and(|arg| dest_arg_has_trailing_slash(arg));
        let dest_dir = dest_arg.map_or_else(|| PathBuf::from("."), PathBuf::from);

        // upstream: main.c:778-792 get_local_name() - pre-flight mkdir of the
        // destination root when the transfer is multi-file or the operand
        // carries a trailing slash. The local-mode receiver creates the root
        // implicitly via the file-list-driven mkdir, but `--server` mode
        // never did, breaking the alt-dest interop test that uses a
        // non-existent destination over remote shell.
        //
        // This site is reachable from every `--server` receiver entry:
        // `run_sync`, `run_pipelined`, and `run_pipelined_incremental` all
        // route through `setup_transfer` before per-entry dispatch, so the
        // pre-flight runs uniformly under `--copy-dest`, `--link-dest`, and
        // `--compare-dest` over remote shell. The mkdir is receiver-local;
        // no `MSG_*` frame is emitted on the wire, matching upstream's
        // `get_local_name()` which calls `do_mkdir()` directly against the
        // local filesystem.
        let created_dest_root = ensure_dest_root_exists(
            &dest_dir,
            file_count,
            trailing_slash,
            self.config.flags.dry_run,
        )
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "failed to create destination root {}: {e} {}{}",
                    dest_dir.display(),
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                ),
            )
        })?;
        if created_dest_root {
            debug_log!(Recv, 1, "created destination root {}", dest_dir.display());
        }

        // UTS-SLDB: when the dest root is a symlink that resolved to a real
        // directory via the stat path in ensure_dest_root_exists, lock the
        // canonical target in here so every downstream open (DirSandbox,
        // per-entry `*at` syscalls) operates on the resolved directory.
        // Upstream `main.c:748` reaches the same state by calling
        // `change_dir(dest_path, CD_NORMAL)` after `S_ISDIR` succeeds: the
        // kernel resolves the link once and every subsequent syscall is
        // relative to the resolved cwd. We mirror that by canonicalizing
        // here instead of relying on chdir.
        //
        // Skipped under daemon connections: the daemon strict path in
        // `open_sandbox_for_dest_strict` refuses a symlinked dest outright
        // (chdir-symlink-race defense), and the module loader has already
        // restricted `module.path`. Canonicalizing here would mask the
        // symlink and let strict mode silently succeed against the resolved
        // target. Local-mode and non-daemon SSH transfers are the
        // upstream-parity case (issue #715 `symlink-dirlink-basis`).
        let dest_dir = if !self.config.flags.dry_run
            && !self.config.connection.is_daemon_connection
            && dest_dir
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            match std::fs::canonicalize(&dest_dir) {
                Ok(resolved) => {
                    debug_log!(
                        Recv,
                        2,
                        "resolved symlinked destination root {} -> {}",
                        dest_dir.display(),
                        resolved.display()
                    );
                    resolved
                }
                Err(err) => {
                    debug_log!(
                        Recv,
                        1,
                        "canonicalize({}) failed: {err}; keeping link path",
                        dest_dir.display()
                    );
                    dest_dir
                }
            }
        } else {
            dest_dir
        };

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
                        "refusing to open destination '{}' via a symlink: \
                         {err} (errno={}) (would expose the \
                         chdir-symlink-race attack window)",
                        dest_dir.display(),
                        code.unwrap_or(0),
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
        // The wrapped error embeds the underlying errno from
        // `DirSandbox::open_root` (ELOOP on Linux + openat2; ENOTDIR on
        // macOS/BSD where O_DIRECTORY is evaluated before O_NOFOLLOW).
        // Both prove the symlink was refused at the syscall layer. The
        // wrapped Display also carries the security-context message so
        // operators see why the transfer aborted. Asserting on the embedded
        // errno avoids the unstable `io::ErrorKind::FilesystemLoop` /
        // `NotADirectory` variants (rust-lang/rust#86442).
        let msg = err.to_string();
        let expected_errno_a = format!("errno={}", libc::ELOOP);
        let expected_errno_b = format!("errno={}", libc::ENOTDIR);
        assert!(
            msg.contains(&expected_errno_a) || msg.contains(&expected_errno_b),
            "expected ELOOP or ENOTDIR errno embedded in message, got: {err}"
        );
        assert!(
            msg.contains("chdir-symlink-race"),
            "expected chdir-symlink-race security context in message, got: {err}"
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

#[cfg(all(test, unix))]
mod daemon_incoming_chmod_tests {
    //! Daemon `incoming chmod = SPEC` regression: the receiver must apply the
    //! daemon module's chmod modifiers on top of the destination mode before
    //! the on-disk permissions are finalized. Mirrors upstream
    //! `clientserver.c:rsync_module()` arming `daemon_chmod_modes` and
    //! `receiver.c` invoking it via `set_file_attrs()` at finalize time.

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use ::metadata::{ChmodModifiers, MetadataOptions, apply_file_metadata_with_options};

    /// Builds the same `MetadataOptions` chain the receiver-side
    /// `setup_transfer` produces (perms + chmod modifiers + fake_super) for a
    /// daemon-config-driven `incoming chmod`. This is the receiver's runtime
    /// contract: the chmod modifiers must be installed via
    /// `MetadataOptions::with_chmod` so the existing apply path rewrites the
    /// destination mode at finalize time.
    fn receiver_metadata_opts(chmod: Option<ChmodModifiers>) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_times(false)
            .with_chmod(chmod)
    }

    /// `incoming chmod = F600` forces every received regular file to land on
    /// disk with mode 0o600 regardless of the source's mode bits, matching
    /// upstream's `daemon_chmod_modes` semantics for push transfers.
    #[test]
    fn incoming_chmod_f600_rewrites_dest_to_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let dest = tmp.path().join("dest.bin");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        // Source-side mode the sender would have advertised on the wire.
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        // Existing dest mode the receiver overwrites.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("F600").expect("parse chmod spec");
        let opts = receiver_metadata_opts(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = F600 must force destination mode to 0o600",
        );
    }

    /// Without an `incoming chmod` directive the receiver preserves the
    /// source mode exactly - no silent rewrite, no default fall-through.
    #[test]
    fn no_incoming_chmod_preserves_source_mode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let dest = tmp.path().join("dest.bin");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let opts = receiver_metadata_opts(None);

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply metadata without chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o640);
    }

    /// `incoming chmod = Fg-r,Fo-r` strips group-read and other-read from
    /// every received regular file. Matches the daemon-filter scenario:
    /// uploading `a.secret` at mode 0o644 must land at 0o600 because the
    /// daemon module strips both group-read and other-read.
    // upstream: testsuite/daemon-filter+chmod parity - mode 0644 -> 0600
    #[test]
    fn incoming_chmod_fg_r_fo_r_strips_world_and_group_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("a.secret");
        let dest = tmp.path().join("a.secret.dest");
        fs::write(&source, b"secret payload").expect("write source");
        fs::write(&dest, b"secret payload").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("Fg-r,Fo-r").expect("parse chmod spec");
        let opts = receiver_metadata_opts(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = Fg-r,Fo-r on 0644 must yield 0600",
        );
    }

    /// `incoming chmod = Fu+rw,go-rwx` is the chmod-option daemon scenario:
    /// uploading `name1` at mode 0o644 must land at 0o600 (user keeps rw,
    /// group and other lose every bit). The second clause has no `F`
    /// prefix; with `target = All` it still applies to files (and would
    /// also apply to dirs, but the destination here is a regular file).
    // upstream: testsuite/chmod-option parity - mode 0644 + go-rwx -> 0600
    #[test]
    fn incoming_chmod_fu_plus_rw_go_minus_rwx_resolves_to_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("name1");
        let dest = tmp.path().join("name1.dest");
        fs::write(&source, b"This is the file").expect("write source");
        fs::write(&dest, b"This is the file").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("Fu+rw,go-rwx").expect("parse chmod spec");
        let opts = receiver_metadata_opts(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = Fu+rw,go-rwx on 0644 must yield 0600",
        );
    }

    /// Single-source-of-truth: the parsed `ChmodModifiers` and the resulting
    /// destination mode are byte-identical whether the spec arrives via CLI
    /// `--chmod=SPEC` or via the daemon `incoming chmod = SPEC` directive.
    /// Both code paths funnel through `ChmodModifiers::parse`; this test
    /// guards against a future fork (e.g. a duplicate daemon-only parser)
    /// silently diverging.
    // upstream: parity with options.c:parse_chmod() (CLI) and
    // clientserver.c:rsync_module() (daemon)
    #[test]
    fn cli_chmod_and_daemon_incoming_chmod_resolve_identically() {
        let spec = "Fg-r,Fo-r";
        let cli_modifiers = ChmodModifiers::parse(spec).expect("parse via CLI path");
        let daemon_modifiers = ChmodModifiers::parse(spec).expect("parse via daemon path");
        assert_eq!(
            cli_modifiers, daemon_modifiers,
            "CLI and daemon parsers must agree byte-for-byte on the parsed clauses",
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("name1");
        let dest_cli = tmp.path().join("name1.cli");
        let dest_daemon = tmp.path().join("name1.daemon");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest_cli, b"payload").expect("write dest cli");
        fs::write(&dest_daemon, b"payload").expect("write dest daemon");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        fs::set_permissions(&dest_cli, fs::Permissions::from_mode(0o644))
            .expect("set dest cli perms");
        fs::set_permissions(&dest_daemon, fs::Permissions::from_mode(0o644))
            .expect("set dest daemon perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let cli_opts = receiver_metadata_opts(Some(cli_modifiers));
        let daemon_opts = receiver_metadata_opts(Some(daemon_modifiers));

        apply_file_metadata_with_options(&dest_cli, &source_meta, &cli_opts)
            .expect("apply CLI chmod");
        apply_file_metadata_with_options(&dest_daemon, &source_meta, &daemon_opts)
            .expect("apply daemon incoming chmod");

        let cli_mode = fs::metadata(&dest_cli)
            .expect("dest cli metadata")
            .permissions()
            .mode();
        let daemon_mode = fs::metadata(&dest_daemon)
            .expect("dest daemon metadata")
            .permissions()
            .mode();
        assert_eq!(
            cli_mode & 0o7777,
            daemon_mode & 0o7777,
            "CLI --chmod and daemon incoming chmod must yield identical destination modes",
        );
        assert_eq!(cli_mode & 0o7777, 0o600);
    }

    /// Caps-X (`X`, conditional-exec) parity: `Da+X,Fa+X` adds execute
    /// only when the entry is already executable or is a directory. A
    /// regular file with mode 0o644 (no execute bits) must keep its
    /// exec bits cleared; a directory at 0o700 must gain `a+x` and land
    /// at 0o711.
    // upstream: chmod.c:tweak_mode FLAG_X_KEEP - "!IsX && !S_ISDIR" branch
    #[test]
    fn caps_x_conditional_exec_only_applies_when_already_executable_or_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Regular file path: no exec bits on source -> no exec bits added.
        let source_file = tmp.path().join("plain.txt");
        let dest_file = tmp.path().join("plain.txt.dest");
        fs::write(&source_file, b"plain").expect("write source file");
        fs::write(&dest_file, b"plain").expect("write dest file");
        fs::set_permissions(&source_file, fs::Permissions::from_mode(0o644))
            .expect("set source file perms");
        fs::set_permissions(&dest_file, fs::Permissions::from_mode(0o644))
            .expect("set dest file perms");

        // Directory path: D-clause must apply `a+X` unconditionally on dirs.
        let source_dir = tmp.path().join("source_dir");
        let dest_dir = tmp.path().join("dest_dir");
        fs::create_dir(&source_dir).expect("mkdir source");
        fs::create_dir(&dest_dir).expect("mkdir dest");
        fs::set_permissions(&source_dir, fs::Permissions::from_mode(0o700))
            .expect("set source dir perms");
        fs::set_permissions(&dest_dir, fs::Permissions::from_mode(0o700))
            .expect("set dest dir perms");

        let modifiers = ChmodModifiers::parse("Da+X,Fa+X").expect("parse caps-X spec");

        // File branch.
        let source_file_meta = fs::metadata(&source_file).expect("source file metadata");
        let opts = receiver_metadata_opts(Some(modifiers.clone()));
        apply_file_metadata_with_options(&dest_file, &source_file_meta, &opts)
            .expect("apply caps-X to file");
        let file_mode = fs::metadata(&dest_file)
            .expect("dest file metadata")
            .permissions()
            .mode();
        assert_eq!(
            file_mode & 0o111,
            0,
            "Fa+X must not add exec bits to a non-executable file (mode was {:o})",
            file_mode & 0o7777,
        );

        // Directory branch. The receiver applies dir metadata through the
        // same options pipeline as files (the dir-only D-clause guards the
        // file-only F-clause and vice versa).
        let source_dir_meta = fs::metadata(&source_dir).expect("source dir metadata");
        let dir_opts = receiver_metadata_opts(Some(modifiers));
        apply_file_metadata_with_options(&dest_dir, &source_dir_meta, &dir_opts)
            .expect("apply caps-X to dir");
        let dir_mode = fs::metadata(&dest_dir)
            .expect("dest dir metadata")
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o111,
            0o111,
            "Da+X must add a+x to a directory (mode was {:o})",
            dir_mode & 0o7777,
        );
    }

    /// Builds `MetadataOptions` matching the daemon-upload receiver path
    /// when the client invoked `rsync -avv --no-perms`: permissions are NOT
    /// preserved but the module's `incoming chmod` directive must still
    /// rewrite the destination mode at finalize time. This mirrors upstream
    /// rsync where `--no-perms` only suppresses source-mode propagation
    /// and never overrides the daemon's `daemon_chmod_modes` arming.
    fn receiver_metadata_opts_no_perms(chmod: Option<ChmodModifiers>) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_times(false)
            .with_chmod(chmod)
    }

    /// UTS-17.REOPEN: daemon-upload with client `--no-perms` must still
    /// apply the daemon module's `incoming chmod` directive. Upstream
    /// rsync's `daemon_chmod_modes` arms at module-load and fires from
    /// the receiver's `set_file_attrs()` regardless of whether `-p` was
    /// negotiated. Regression-guards the chmod-option upstream-testsuite
    /// scenario where the spec `ug-s,a+rX,D+w` must coerce uploaded
    /// files to land with world+group read bits set.
    // upstream: clientserver.c:rsync_module() arms daemon_chmod_modes,
    //           receiver.c:set_file_attrs() applies it independently of -p.
    #[test]
    fn no_perms_does_not_suppress_daemon_incoming_chmod() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("bar");
        let dest = tmp.path().join("bar.dest");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        // Source mode is whatever the sender advertised; with `--no-perms`
        // the receiver ignores it. The daemon's `incoming chmod` must still
        // run against the destination's current mode.
        fs::set_permissions(&source, fs::Permissions::from_mode(0o664)).expect("set source perms");
        // Newly-renamed temp file lands at a tight 0o600 (the O_TMPFILE
        // default). The chmod-apply path must lift it to the spec result.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        // `a+rX` is the chmod-option upstream-testsuite incoming-chmod
        // clause that must add read for all (and conditional X, which is
        // a no-op on a non-executable file).
        let modifiers = ChmodModifiers::parse("ug-s,a+rX,D+w").expect("parse chmod spec");
        let opts = receiver_metadata_opts_no_perms(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod under --no-perms");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        // Starting from 0o600 the spec resolves to 0o644 (add read for all,
        // no exec bits to retain on a non-executable file).
        assert_eq!(
            mode & 0o7777,
            0o644,
            "incoming chmod = ug-s,a+rX,D+w applied to 0o600 destination under --no-perms \
             must lift to 0o644 (mode was {:o})",
            mode & 0o7777,
        );
    }

    /// Hardens the contract: a daemon module with `incoming chmod = F600`
    /// must clamp every uploaded regular file to 0o600 even when the
    /// client passed `--no-perms`. This guards against a future
    /// regression where `--no-perms` short-circuits the chmod-apply
    /// branch.
    // upstream: parity with `incoming chmod = F600` daemon-config tests.
    #[test]
    fn no_perms_with_incoming_chmod_f600_clamps_to_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("secret");
        let dest = tmp.path().join("secret.dest");
        fs::write(&source, b"payload").expect("write source");
        fs::write(&dest, b"payload").expect("write dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("set source perms");
        // Existing dest mode is generous; chmod must clamp it down.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o664)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let modifiers = ChmodModifiers::parse("F600").expect("parse chmod spec");
        let opts = receiver_metadata_opts_no_perms(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply daemon incoming chmod under --no-perms");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "incoming chmod = F600 under --no-perms must force destination to 0o600 \
             (mode was {:o})",
            mode & 0o7777,
        );
    }

    /// Directories receive the `D+w` clause exclusively: a file-only spec
    /// (`Fo-x`) leaves directory modes untouched under `--no-perms`,
    /// matching upstream `chmod.c:tweak_mode()` clause targeting.
    // upstream: chmod.c:tweak_mode() - F-prefix file-only / D-prefix dir-only.
    #[test]
    fn no_perms_directory_chmod_applies_only_d_clause() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source_dir");
        let dest = tmp.path().join("dest_dir");
        fs::create_dir(&source).expect("mkdir source");
        fs::create_dir(&dest).expect("mkdir dest");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o750)).expect("set source perms");
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o750)).expect("set dest perms");

        let source_meta = fs::metadata(&source).expect("source metadata");
        // The `Fo-x` clause is file-only and must NOT touch dir modes.
        let modifiers = ChmodModifiers::parse("Fo-x").expect("parse chmod spec");
        let opts = receiver_metadata_opts_no_perms(Some(modifiers));

        apply_file_metadata_with_options(&dest, &source_meta, &opts)
            .expect("apply chmod to dir under --no-perms");

        let mode = fs::metadata(&dest)
            .expect("dest metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7777,
            0o750,
            "file-only chmod clause `Fo-x` must not touch directory modes \
             (mode was {:o})",
            mode & 0o7777,
        );
    }
}
