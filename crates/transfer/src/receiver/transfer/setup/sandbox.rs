//! Destination-root sandbox carriers for the receiver transfer setup.
//!
//! Opens the destination root as a [`fast_io::DirSandbox`] so the per-entry
//! `*at` syscall cutover sites can ride a sandboxed dirfd. The strict variant
//! propagates symlink-class refusals as a transfer error for daemon receivers
//! without chroot (the chdir-symlink-race defence).

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::sync::Arc;

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
pub(super) fn open_sandbox_for_dest_strict(
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

/// Open a daemon receiver's destination as an operator anchor plus a
/// confined peer tail.
///
/// `dest_dir` reaches this module as a single flattened string built by
/// `resolve_receiver_dest_path` as `module_root.join(peer_tail)`
/// (`client_args/path_resolution.rs`). The two halves carry different
/// trust: the module root is named by the operator in `oc-rsyncd.conf`,
/// while the tail is whatever the client sent on the wire. Applying one
/// resolve policy to the fused string cannot express that, so this splits
/// them back apart and gives each half the mechanism upstream gives it:
///
/// - the anchor is opened with a plain `open(2)`, because an operator who
///   writes `path = /srv/backup` has authorised every component of it,
///   symlinked or not;
/// - each tail component is walked with `RESOLVE_BENEATH`, so an in-tree
///   symlink is followed and anything leaving the module fails `EXDEV`.
///
/// Re-anchoring the tail is what keeps this a bug fix rather than a hole:
/// opening the root plainly and then re-joining the tail into a path-based
/// open would let a symlinked tail component walk straight out of the
/// module.
///
/// `ENOENT` stays a soft failure (`Ok(None)`) so a first-run push still
/// creates the tree through `ensure_relative_parents`. An escape refusal
/// is fatal.
///
/// # Upstream Reference
///
/// - `syscall.c:85-90` - `open_anchor_dirfd()` uses a plain `openat`; the
///   comment at `syscall.c:3189-3193` records why ("Absolute basedir:
///   operator-trusted").
/// - `syscall.c:2891` - `ds_descend()` walks the untrusted remainder, and
///   `syscall.c:2961` splices a *relative* in-tree symlink target back
///   into the walk rather than refusing it.
/// - `main.c:765` - the daemon reaches the same state by `change_dir()`
///   onto the module root before serving.
#[cfg(unix)]
pub(super) fn open_sandbox_for_dest_anchored(
    module_root: &std::path::Path,
    dest_dir: &std::path::Path,
) -> io::Result<Option<Arc<fast_io::DirSandbox>>> {
    let peer_tail = dest_dir.strip_prefix(module_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "destination '{}' is not under module root '{}'",
                dest_dir.display(),
                module_root.display(),
            ),
        )
    })?;

    match fast_io::DirSandbox::open_dest_anchor(module_root, peer_tail) {
        Ok(sandbox) => Ok(Some(Arc::new(sandbox))),
        Err(err) => {
            let code = err.raw_os_error();
            if matches!(code, Some(libc::EXDEV)) {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "refusing destination '{}': the client-supplied path \
                         '{}' escapes module root '{}' (errno={})",
                        dest_dir.display(),
                        peer_tail.display(),
                        module_root.display(),
                        code.unwrap_or(0),
                    ),
                ));
            }
            if matches!(code, Some(libc::ENOENT)) {
                // First-run push: the tail is created later by
                // `ensure_relative_parents`. Ordinary, and not worth an
                // operator's attention.
                logging::debug_log!(
                    Recv,
                    2,
                    "DirSandbox::open_dest_anchor({}, {}) got ENOENT; \
                     the tail will be created during the transfer",
                    module_root.display(),
                    peer_tail.display()
                );
                return Ok(None);
            }
            // Anything else means a daemon receiver just lost its anchored
            // dirfd and will fall back to path-based syscalls - it keeps
            // running, but without the per-component confinement its role
            // is supposed to have. Upstream confines exactly this role
            // (`use_secure_symlinks = am_daemon && !am_chrooted`,
            // clientserver.c:1018), so degrading is worth saying out loud.
            //
            // Warning, not debug: `debug_log!` here is unreachable at every
            // verbosity an operator can actually set, which made this
            // condition unobservable in the field. upstream: FWARNING is
            // dispatched by rwrite() (log.c:341) and is not verbosity-gated.
            logging::warn_log!(
                "receiver: could not anchor destination '{}' under module \
                 root '{}': {err}; continuing without path confinement",
                peer_tail.display(),
                module_root.display()
            );
            Ok(None)
        }
    }
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

    /// The operator's own module root may sit behind a symlink - `path =
    /// /srv/backup` where `/srv -> /mnt/srv` is an ordinary layout. Upstream
    /// opens that anchor with a plain `openat` (`syscall.c:85-90`), so every
    /// transfer through it must succeed.
    ///
    /// ⚠ The symlink is planted **explicitly**. A bare `TempDir` proves
    /// nothing about symlinked ancestors on either CI platform: on macOS
    /// `/tmp -> private/tmp` means every tempdir already has one (and the
    /// leaf-only non-Linux arm never inspects interior components anyway),
    /// while on Linux CI tempdirs sit under a real `/tmp` so the interior
    /// check is never reached. The platform that has the condition cannot
    /// detect it; the platform that can detect it never has it.
    #[test]
    fn anchored_mode_accepts_a_symlinked_module_root() {
        let (_keep, root) = canonical_tempdir();
        let real_store = root.join("mnt-srv");
        std::fs::create_dir(&real_store).expect("create real store");
        std::fs::create_dir(real_store.join("backup")).expect("create module dir");

        // The operator's configured root reaches its target through a link.
        let srv = root.join("srv");
        symlink(&real_store, &srv).expect("symlink srv -> mnt-srv");
        let module_root = srv.join("backup");

        let result = open_sandbox_for_dest_anchored(&module_root, &module_root)
            .expect("a symlinked module root is operator-trusted and must open");
        assert!(
            result.is_some(),
            "plain-open anchor must hand back a sandbox; a resolve policy \
             applied to the operator's own root refuses every transfer"
        );

        // Control: the same fixture through the pre-fix path. Without this
        // the assertion above passes on a build that never had the bug, and
        // the test would be evidence of nothing.
        //
        // Linux-only. The bug needs an *interior* component check, which
        // only the `openat2(RESOLVE_NO_SYMLINKS)` arm performs; the
        // non-Linux arm is leaf-only `O_NOFOLLOW`, and the leaf here
        // (`backup`) is a real directory, so the old path succeeds there
        // for a reason unrelated to the fix.
        #[cfg(target_os = "linux")]
        {
            let old = open_sandbox_for_dest_strict(&module_root, true);
            assert!(
                old.is_err(),
                "control failed: the pre-fix path accepted this layout, so \
                 the assertion above cannot be evidence of the fix"
            );
        }
    }

    /// True when the peer-tail walk can tell an in-tree symlink from an
    /// escape.
    ///
    /// Only `openat2(RESOLVE_BENEATH)` can: it follows a component and then
    /// reports `EXDEV` if resolution left the anchor. The portable fallback
    /// is `openat(O_DIRECTORY | O_NOFOLLOW)` per component
    /// (`fast_io/src/dir_sandbox/mod.rs` `openat_dir` -> `openat_nofollow`),
    /// which refuses *every* symlink component without resolving it, so the
    /// two cases are indistinguishable there.
    ///
    /// Measured 2026-08-14 on macOS 15 (aarch64), both fixtures below:
    ///
    /// ```text
    /// DirSandbox::open_dest_anchor(module, "link")    -> ENOTDIR (20)
    /// DirSandbox::open_dest_anchor(module, "escape")  -> ENOTDIR (20)
    /// ```
    ///
    /// Identical errno for the case that must be followed and the case that
    /// must be refused. That is the whole divergence, and it is why the two
    /// tests below branch instead of asserting one answer.
    fn peer_tail_walk_can_resolve_symlinks() -> bool {
        cfg!(target_os = "linux") && fast_io::openat2_supported()
    }

    /// The peer half keeps `RESOLVE_BENEATH`. A client-supplied tail whose
    /// component is a symlink out of the module must not open, or the
    /// plain-open anchor above would have turned an availability bug into a
    /// module escape.
    ///
    /// ⚠ KNOWN GAP, not intended behaviour. On a platform without
    /// `openat2` the walk cannot distinguish this from an in-tree symlink,
    /// so `open_sandbox_for_dest_anchored` takes its `_ =>` arm: it warns
    /// and returns `Ok(None)`, and the receiver then falls back to
    /// **path-based** syscalls that resolve straight through the planted
    /// symlink. The escape is loud but it is not refused. Closing that needs
    /// the per-component resolver (`ds_descend`, `syscall.c:2891-2965`),
    /// which follows a relative in-tree target and refuses an absolute or
    /// escaping one - tracked by task 604. This test pins both arms so the
    /// gap cannot widen unnoticed and so the Linux contract cannot regress.
    #[test]
    fn anchored_mode_refuses_a_peer_tail_that_escapes_the_module() {
        let (_keep, root) = canonical_tempdir();
        let module_root = root.join("module");
        std::fs::create_dir(&module_root).expect("create module root");
        let outside = root.join("outside");
        std::fs::create_dir(&outside).expect("create outside dir");

        // Relative target: an absolute one is refused under RESOLVE_BENEATH
        // even when it lands inside, so it would pass for the wrong reason.
        symlink("../outside", module_root.join("escape")).expect("symlink escape -> ../outside");

        let outcome = open_sandbox_for_dest_anchored(&module_root, &module_root.join("escape"));

        if !peer_tail_walk_can_resolve_symlinks() {
            let degraded = outcome
                .expect("the O_NOFOLLOW walk reports ENOTDIR, which is not the EXDEV escape arm");
            assert!(
                degraded.is_none(),
                "without openat2 the escape is not refused; it degrades to the \
                 unconfined path-based fall-back (task 604)"
            );
            return;
        }

        let err = outcome.expect_err("a peer tail leaving the module must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("escapes module root"),
            "expected the escape refusal, got: {err}"
        );
        assert!(
            msg.contains(&format!("errno={}", libc::EXDEV)),
            "escape must be refused as an escape (EXDEV), not merely as \
             'there was a symlink', got: {err}"
        );
    }

    /// The other half of the same policy: a symlinked *subdirectory* inside
    /// the module is ordinary content and must transfer. upstream:
    /// `ds_descend()` splices a relative in-tree target back into the walk
    /// (`syscall.c:2961`) rather than refusing it.
    ///
    /// ⚠ KNOWN GAP, not intended behaviour, same cause as the escape test
    /// above and the same owner (task 604). Without `openat2` the walk
    /// refuses the symlinked component, so the daemon receiver loses its
    /// anchored dirfd and continues unconfined. Unlike the escape case this
    /// costs confinement rather than granting an escape, but it is the same
    /// missing mechanism.
    #[test]
    fn anchored_mode_follows_an_in_tree_symlinked_subdirectory() {
        let (_keep, root) = canonical_tempdir();
        let module_root = root.join("module");
        std::fs::create_dir(&module_root).expect("create module root");
        std::fs::create_dir(module_root.join("real")).expect("create real subdir");
        symlink("real", module_root.join("link")).expect("symlink link -> real");

        let result = open_sandbox_for_dest_anchored(&module_root, &module_root.join("link"))
            .expect("an in-tree symlinked subdirectory must not be refused");

        if peer_tail_walk_can_resolve_symlinks() {
            assert!(
                result.is_some(),
                "a symlinked subdirectory inside the module is ordinary content"
            );
        } else {
            assert!(
                result.is_none(),
                "without openat2 the walk refuses the symlinked component and \
                 falls back unconfined (task 604); if this now returns a \
                 sandbox the resolver has landed and both arms should assert \
                 is_some()"
            );
        }
    }

    #[test]
    fn anchored_mode_soft_fails_when_the_tail_is_missing() {
        let (_keep, root) = canonical_tempdir();
        let module_root = root.join("module");
        std::fs::create_dir(&module_root).expect("create module root");

        let result = open_sandbox_for_dest_anchored(&module_root, &module_root.join("not-yet"))
            .expect("ENOENT must stay a soft failure - first-run push mkdirs later");
        assert!(result.is_none());
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
