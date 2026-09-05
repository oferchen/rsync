//! Destination-root sandbox carriers for the receiver transfer setup.
//!
//! Opens the destination root as a [`fast_io::DirSandbox`] so the per-entry
//! `*at` syscall cutover sites can ride a sandboxed dirfd. The split mirrors
//! upstream's `am_daemon` gate: a daemon server takes the anchored variant,
//! which confines the peer-supplied tail beneath the operator's module root
//! (the chdir-symlink-race defence), and every other receiver takes the plain
//! variant, which upstream leaves unconfined.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::sync::Arc;

/// Whether this session opted out of receiver-side symlink confinement.
///
/// The `insecure links` opt-out is not a sender-only concession: it disables
/// the secure resolver on the RECEIVER side too, so an opted-out module
/// follows a pre-existing in-module symlink exactly as pre-3.4.3 rsync did.
/// Declining the sandbox is oc's spelling of taking upstream's plain-open arm:
/// the `*_via_sandbox_or_fallback` helpers then route through
/// `fast_io::confined_fallback`, whose own first arm is the same opt-out.
///
/// # Upstream Reference
///
/// - `syscall.c:100-114` - `secure_relpath_active()` short-circuits to `0`
///   under `symlink_optout_allowed()` before any of its other tests, and the
///   comment above it records why the receiver is included.
/// - `receiver.c:428`, `receiver.c:1204` - the two receiver consumers, each
///   choosing the secure open only when that gate is active.
/// - `syscall.c:122-127` - `symlink_optout_allowed()` reads the served
///   module's `insecure links` for a daemon and `--insecure-links` otherwise;
///   `fast_io::confinement::session_optout_allowed` is oc's port of it.
#[cfg(unix)]
fn confinement_opted_out() -> bool {
    fast_io::confinement::session_optout_allowed()
}

/// Open the destination root as a [`fast_io::DirSandbox`] carrier for a
/// receiver that applies no confinement.
///
/// Returns `Some(Arc<DirSandbox>)` when the path exists and resolves to
/// a non-symlink directory the receiver can open. Returns `None` for any
/// other outcome (path does not exist yet, path is a symlink, EACCES,
/// etc.) so the receiver keeps running on the existing path-based
/// fall-backs while the SEC-1.f-j cutover lands site by site.
///
/// This is the arm for every receiver upstream leaves unconfined -
/// local, SSH server, and the client half of an `rsync://` pull. There a
/// failed sandbox open is not a degraded security posture but the normal
/// state: `secure_basis_open()` takes its `if (!am_daemon || ...)` branch
/// and does a plain `do_open`, so following a symlinked destination is the
/// behaviour, not a fall-back from it. The daemon server takes
/// [`open_sandbox_for_dest_anchored`] instead, which is where a failure
/// does mean lost confinement and says so out loud.
///
/// Failures are logged at `Debug` level only; they are expected on
/// first-run transfers where the destination is created later in
/// `ensure_relative_parents` / `create_directories`. `--debug=recv2`
/// reaches them, matching upstream, whose `-v` ladder likewise never
/// raises RECV past 1 (`options.c:248` `debug_verbosity[3]`).
///
/// # Upstream Reference
///
/// - `clientserver.c:1093` - `use_secure_symlinks = am_daemon &&
///   (!am_chrooted || module_dirlen)`
/// - `receiver.c:152` - the `if (!am_daemon || ...)` plain-open arm of
///   `secure_basis_open()`
/// - `syscall.c:136` - `confinement_root()` is `module_dir` only for a daemon
#[cfg(unix)]
pub(super) fn open_sandbox_for_dest(
    dest_dir: &std::path::Path,
) -> Option<Arc<fast_io::DirSandbox>> {
    if confinement_opted_out() {
        return None;
    }
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
    // The opt-out disables the resolver, so the EXDEV refusal below must not
    // fire either: `insecure links = yes` promises the pre-3.4.3 resolver
    // verbatim, and refusing a destination the legacy open would have followed
    // is not "confine less", it is a different behaviour entirely.
    if confinement_opted_out() {
        return Ok(None);
    }

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
            // (`use_secure_symlinks = am_daemon && (!am_chrooted ||
            // module_dirlen)`, clientserver.c:1093), so degrading is worth
            // saying out loud.
            //
            // Warning, not debug, because a daemon silently losing its
            // confinement is an operator-facing condition, not a trace. Not
            // because `debug_log!` would be unreachable: `--debug=recv2`
            // reaches level 2 (measured). What the `-v` ladder alone cannot
            // reach is upstream's own arrangement - `debug_verbosity[3]`
            // (options.c:248) sets RECV to 1 and no later level raises it -
            // so a debug channel here would be invisible to `-vvvvv`, which
            // is what an operator actually reaches for. upstream: FWARNING is
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

// upstream: clientserver.c:1093 - `use_secure_symlinks = am_daemon &&
// (!am_chrooted || module_dirlen)`, the gating the chdir-symlink-race fix
// mirrors. Tests below verify the anchored daemon branch confines the
// peer-supplied tail beneath the operator's module root, while the plain
// branch - every receiver upstream leaves unconfined - keeps its soft-fail.
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

    /// A symlinked destination root must never become a hard refusal on the
    /// unconfined path. `DirSandbox::open_root` declines to open through the
    /// link, so the receiver takes `None` and keeps running on the path-based
    /// syscalls - which follow the link, exactly as upstream's
    /// `secure_basis_open()` plain-open arm does for `am_daemon == 0`
    /// (receiver.c:152).
    ///
    /// Returning `Err` here instead is what made `oc-rsync -a rsync://h/m/ DEST`
    /// exit 23 on a symlinked `DEST` that real rsync 3.5.0 transfers into.
    #[test]
    fn unconfined_open_soft_fails_for_a_symlinked_destination() {
        let (_keep, root) = canonical_tempdir();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).expect("create outside dir");
        let subdir = root.join("subdir");
        symlink(&outside, &subdir).expect("symlink subdir -> outside");

        assert!(
            open_sandbox_for_dest(&subdir).is_none(),
            "an unconfined receiver must soft-fail to the path-based syscalls, \
             not refuse the transfer"
        );
    }

    #[test]
    fn unconfined_open_accepts_a_real_directory_destination() {
        let (_keep, root) = canonical_tempdir();
        let real = root.join("realdir");
        std::fs::create_dir(&real).expect("create real dir");

        assert!(
            open_sandbox_for_dest(&real).is_some(),
            "the unconfined open must hand back a sandbox when the dest is a \
             real dir; otherwise the *at cutover never engages"
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
            assert!(
                open_sandbox_for_dest(&module_root).is_none(),
                "control failed: the fused single-policy open accepted this \
                 layout, so the assertion above cannot be evidence of the fix"
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
    pub(super) fn peer_tail_walk_can_resolve_symlinks() -> bool {
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
    fn unconfined_open_soft_fails_when_destination_is_missing() {
        let (_keep, root) = canonical_tempdir();
        let missing = root.join("not-yet-created");

        assert!(
            open_sandbox_for_dest(&missing).is_none(),
            "ENOENT must be a soft failure - first-run push will mkdir later"
        );
    }
}

/// The `insecure links` opt-out reaches the RECEIVER, not just the sender.
///
/// Upstream's `secure_relpath_active()` (`syscall.c:100-114`) short-circuits to
/// `0` under `symlink_optout_allowed()` before every other test, and the
/// comment above it says why in upstream's own words: without that, "an
/// opted-out module still confined receiver writes/stats through a pre-existing
/// in-module symlink -- failing to match the pre-3.4.3 behaviour the opt-out
/// promises". Declining the sandbox is oc's spelling of the plain-open arm.
///
/// The confinement session is process-global (upstream reads the same answer
/// from globals). nextest runs one process per test, which is what keeps these
/// cells sound; they are not safe under a shared-process runner.
#[cfg(all(test, unix))]
mod insecure_links_optout_tests {
    use super::symlink_race_tests::peer_tail_walk_can_resolve_symlinks;
    use super::*;
    use fast_io::confinement::{ModuleInsecureLinks, ModuleState, install_daemon_session};
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use tempfile::tempdir;

    fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().expect("tempdir");
        let canon = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
        (dir, canon)
    }

    /// A served module at the requested opt-out setting. Everything except
    /// `insecure_links` is held fixed, so a paired cell differs in exactly one
    /// variable - which is what makes the pair an A/B rather than two
    /// unrelated fixtures.
    fn serve_module(root: &Path, insecure_links: bool) {
        install_daemon_session(ModuleState {
            root: Some(root.to_path_buf()),
            chrooted: false,
            selected: true,
            insecure_links: ModuleInsecureLinks::from_module_config(insecure_links),
        });
    }

    /// `insecure links = yes` must decline the anchored sandbox outright, so
    /// every later `*at` site falls to the plain path syscalls upstream uses.
    #[test]
    fn opted_out_module_declines_the_anchored_sandbox() {
        let (_keep, root) = canonical_tempdir();
        let module_root = root.join("module");
        std::fs::create_dir(&module_root).expect("create module root");

        serve_module(&module_root, true);

        let sandbox = open_sandbox_for_dest_anchored(&module_root, &module_root)
            .expect("the opt-out is a policy decision, never an error");
        assert!(
            sandbox.is_none(),
            "an opted-out module must take upstream's plain-open arm; holding a \
             confined dirfd here keeps confining the writes the opt-out exists \
             to un-confine"
        );
    }

    /// Non-vacuity companion: the SAME fixture at the default setting still
    /// opens a sandbox. Without this the cell above would also pass on a build
    /// where the anchored open simply never succeeds.
    #[test]
    fn a_module_at_the_default_setting_still_opens_the_anchored_sandbox() {
        let (_keep, root) = canonical_tempdir();
        let module_root = root.join("module");
        std::fs::create_dir(&module_root).expect("create module root");

        serve_module(&module_root, false);

        let sandbox = open_sandbox_for_dest_anchored(&module_root, &module_root)
            .expect("a real module root must open");
        assert!(
            sandbox.is_some(),
            "confinement is the default; if this arm hands back None the \
             opt-out cell above proves nothing"
        );
    }

    /// The refusal itself is what the opt-out has to suppress. An escaping
    /// peer tail is a hard `Err` at the default setting and must become the
    /// plain-open arm under the opt-out - the single behaviour
    /// `daemon-symlink-escape-matrix` asserts against a live rsync 3.2.7.
    ///
    /// ⚠ Keyed on the walk's resolver: without `openat2` the `O_NOFOLLOW` walk
    /// reports `ENOTDIR` and already degrades to `Ok(None)` (task 604), so the
    /// escape arm cannot discriminate there. The pair above does, on every
    /// platform, which is why it is not conditioned.
    #[test]
    fn opted_out_module_stops_refusing_an_escaping_peer_tail() {
        if !peer_tail_walk_can_resolve_symlinks() {
            return;
        }

        let (_keep, root) = canonical_tempdir();
        let module_root = root.join("module");
        std::fs::create_dir(&module_root).expect("create module root");
        std::fs::create_dir(root.join("outside")).expect("create outside dir");
        symlink("../outside", module_root.join("escape")).expect("symlink escape -> ../outside");
        let escaping_tail = module_root.join("escape");

        serve_module(&module_root, false);
        let refused = open_sandbox_for_dest_anchored(&module_root, &escaping_tail)
            .expect_err("control: the default setting must refuse the escape");
        assert!(
            refused.to_string().contains("escapes module root"),
            "control must fail as an escape, got: {refused}"
        );

        serve_module(&module_root, true);
        let allowed = open_sandbox_for_dest_anchored(&module_root, &escaping_tail)
            .expect("the opt-out restores the legacy follow, so no refusal");
        assert!(
            allowed.is_none(),
            "the opt-out takes the plain-open arm rather than a confined dirfd"
        );
    }
}
