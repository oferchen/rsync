//! Receiver-side confinement shared by the `--server` process and a local
//! copy's receiver thread.

use crate::server::{ServerConfig, ServerRole};
use engine::local_copy::{ReferenceDirectory, ReferenceDirectoryKind};

/// Prepares a server receiver's process before any transfer work: checks the
/// basis dirs and engages the oc-only Landlock allowlist on the calling thread.
///
/// Landlock confines the calling thread and every thread it spawns afterwards,
/// so a receiver must call this before it creates any worker.
pub fn confine_receiver(config: &ServerConfig) {
    let role = config.role;
    // Deviation from upstream (oc-only hardening): engage the Landlock allowlist
    // on the receiver-side `--server` path before any token-loop work. The lsh.sh-style invocation
    // exercises upstream `chdir-symlink-race` and `bare-do-open-symlink-race`
    // tests that swap a destination subdir for a symlink pointing outside the
    // requested root; without a kernel-enforced ruleset the receiver follows
    // the symlink and chmod's a file outside the destination tree. We confine
    // the calling thread to the (already-canonicalised, pre-flight-mkdir'd)
    // destination root so any subsequent path-based syscall that escapes the
    // tree gets EACCES from the kernel.
    //
    // Apply only when the receiver actually has a destination root to confine
    // to: the sender role and stat-only invocations have no write target, and
    // engaging an empty allowlist would deny their reads. Landlock is
    // defence-in-depth here: every outcome degrades gracefully to the
    // confined *at* helpers, but a failed or weakened install is surfaced through
    // `surface_landlock_outcome` rather than silently dropped or (as an
    // earlier revision did) escalated into a hard refusal - the abort arm
    // was explicitly rejected in favour of degrade-visibly.
    if role == ServerRole::Receiver {
        // upstream: main.c:1259 - the SERVER receiver runs `check_alt_basis_dirs()`
        // once the destination is known. This is the push direction: the client
        // is the sender and forwards the basis-dir args to us
        // (`options.c:3114-3123` emits them only when the server receives), so
        // the values are already in our own decoded argv and no per-connection
        // plumbing is involved. Warn-only, exit code untouched.
        if let Some(dest) = config.args.last() {
            crate::client::check_alt_basis_dirs(
                &config.reference_directories,
                std::path::Path::new(dest),
            );
        }

        if let Some(dest) = config.args.last() {
            let dest_path = std::path::PathBuf::from(dest);
            if let Some(root) = landlock_root_for_dest(&dest_path) {
                use fast_io::landlock::{is_supported, restrict_to_paths};
                if !is_supported() {
                    // AVAILABILITY, not failure: pre-5.13 kernels, a build
                    // without the `landlock` feature, and every non-Linux
                    // target land here. It is the normal state on those
                    // hosts, so a warning would be noise - debug only, at
                    // the level `-vvv` or `--debug=landlock` reaches.
                    logging::debug_log!(
                        Landlock,
                        1,
                        "landlock: unavailable on this kernel or build; SEC-1 *at* helpers remain the sole defense"
                    );
                } else {
                    let canonical_root = root.clone();
                    let mut allowed = vec![root];

                    // A remote files-from path (upstream
                    // `options.c:3145-3146` -> server gets `--files-from <path>`)
                    // sits outside the destination tree. The receiver
                    // opens it in `forward_files_from_to_sender` to push
                    // filenames back to the sender; the landlock allowlist
                    // must include the file's parent or the open fails
                    // with EACCES (observed as the
                    // "Permission denied (os error 13)" branch when the
                    // receiver reached the new forwarder).
                    if let Some(path) = config.file_selection.files_from_path.as_deref()
                        && path != "-"
                    {
                        let p = std::path::PathBuf::from(path);
                        if let Some(canon) = p
                            .canonicalize()
                            .ok()
                            .or_else(|| p.parent().and_then(|d| d.canonicalize().ok()))
                        {
                            allowed.push(canon);
                        }
                    }

                    // upstream: receiver.c:413 open_tmpfile() - an
                    // operator `--temp-dir` may sit outside the destination,
                    // even on another filesystem, and the receiver creates,
                    // copies and unlinks its temp files there. Without it in
                    // the allowlist every mkstemp fails EACCES. The path is
                    // resolved the way the receiver resolves it: as given,
                    // relative to the working directory.
                    if let Some(canon) = config
                        .temp_dir
                        .as_deref()
                        .and_then(|dir| dir.canonicalize().ok())
                    {
                        allowed.push(canon);
                    }

                    // upstream: generator.c:1745 - with --keep-dirlinks the
                    // receiver follows a destination symlink that resolves to a
                    // directory and writes through it, which upstream permits
                    // even when the target lives outside the transfer root. The
                    // Landlock allowlist would otherwise deny those writes
                    // (EACCES), so extend it with the canonical targets of any
                    // pre-existing destination symlink-to-directory entries. Only
                    // symlinks already present before the transfer are added, so
                    // the escape defense against a mid-transfer symlink swap is
                    // preserved for paths the user did not pre-establish.
                    if config.flags.keep_dirlinks {
                        collect_keep_dirlink_targets(&canonical_root, &mut allowed);
                    }

                    // upstream: backup.c:65-136 and receiver.c:820-849 - a
                    // --backup-dir or absolute --partial-dir may sit outside
                    // the destination; the receiver renames files into it and
                    // creates it on first use, so it needs write rights.
                    let anchors = landlock_anchors(&dest_path);
                    let staging = config
                        .backup_dir
                        .iter()
                        .map(std::path::PathBuf::from)
                        .chain(
                            config
                                .partial_dir
                                .iter()
                                .filter(|p| p.is_absolute())
                                .cloned(),
                        );
                    for dir in staging {
                        allowed.extend(
                            anchored_candidates(&dir, &anchors)
                                .iter()
                                .filter_map(|candidate| staging_write_root(candidate)),
                        );
                    }

                    let (readable, link_sources) =
                        alt_basis_roots(&config.reference_directories, &anchors);
                    let allowed_refs: Vec<&std::path::Path> =
                        allowed.iter().map(|p| p.as_path()).collect();
                    let readable_refs: Vec<&std::path::Path> =
                        readable.iter().map(|p| p.as_path()).collect();
                    let link_source_refs: Vec<&std::path::Path> =
                        link_sources.iter().map(|p| p.as_path()).collect();
                    let outcome =
                        restrict_to_paths(&allowed_refs, &readable_refs, &link_source_refs);
                    surface_landlock_outcome(&outcome, &canonical_root, &allowed_refs);
                }
            }
        }
    }

}

/// Resolves the Landlock allowlist root for a receiver's destination operand.
///
/// The root must be the **directory the receiver writes into**, never the
/// operand itself. Upstream builds every temp name inside the destination's own
/// directory - `get_tmpname()` (upstream: receiver.c:322) splits `fname` at its
/// last `/` and prefixes the temp basename there - so that directory is always
/// required, whether or not the destination file already exists.
///
/// Canonicalising the operand alone is not enough: when it names an **existing
/// regular file** `canonicalize()` succeeds on the file, and Landlock's
/// `PathBeneath` rule over a regular file grants that one file and nothing else.
/// The receiver's `mkstemp` of a sibling temp then fails with `EACCES`. Taking
/// the parent in that case makes an existing destination behave like one that
/// does not exist yet, which already resolved to the parent.
/// The directories a relative `--*-dest`, `--backup-dir` or `--partial-dir`
/// may be anchored on, each with upstream's `dry_run > 1` flag.
///
/// The receiver anchors those on `curr_dir` (main.c:898-911), which
/// `get_local_name()` fixes only once the file list says what arrives
/// (main.c:744-858): the destination itself, or its parent for a single file.
/// The Landlock ruleset goes in before that, so it covers every anchor the
/// destination still admits. A missing destination is anchored as a dry run's
/// is, so `../alt` names the sibling without the directory existing yet.
fn landlock_anchors(dest: &std::path::Path) -> Vec<(std::path::PathBuf, bool)> {
    let trailing_slash = dest
        .as_os_str()
        .to_string_lossy()
        .ends_with(std::path::is_separator);
    let mut anchors = Vec::with_capacity(2);
    if dest.is_dir() {
        anchors.push((dest.to_path_buf(), false));
        return anchors;
    }
    if !dest.exists() {
        anchors.push((dest.to_path_buf(), true));
    }
    if !trailing_slash && let Some(parent) = dest.parent() {
        anchors.push((parent.to_path_buf(), false));
    }
    anchors
}

/// Canonical forms of `path` anchored on each of `anchors`.
fn anchored_candidates(
    path: &std::path::Path,
    anchors: &[(std::path::PathBuf, bool)],
) -> Vec<std::path::PathBuf> {
    let reference = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, path);
    anchors
        .iter()
        .map(|(anchor, pending)| reference.anchored_at(anchor, *pending).path)
        .collect()
}

/// The `--*-dest` directories the `--server` receiver may open, as
/// (read-only roots, link-source roots).
///
/// upstream: generator.c:1073 `try_dests_reg()` - the receiver opens a basis
/// in a `--compare-dest` / `--copy-dest` dir or copies from it
/// (`copy_altdest_file()`), and hard-links out of a `--link-dest` dir. A basis
/// that resolves to nothing gets no rule: the receiver cannot open it either,
/// and `check_alt_basis_dirs()` has already warned.
fn alt_basis_roots(
    references: &[ReferenceDirectory],
    anchors: &[(std::path::PathBuf, bool)],
) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let (links, reads): (Vec<_>, Vec<_>) = references
        .iter()
        .partition(|reference| reference.kind == ReferenceDirectoryKind::Link);
    let canonical = |set: Vec<&ReferenceDirectory>| -> Vec<std::path::PathBuf> {
        set.into_iter()
            .flat_map(|reference| anchored_candidates(&reference.path, anchors))
            .filter_map(|candidate| candidate.canonicalize().ok())
            .collect()
    };
    (canonical(reads), canonical(links))
}

/// The write rule for a staging directory the receiver may create later.
///
/// upstream creates `--backup-dir` (backup.c:128 `do_mkdir_at()` in
/// `make_bak_dir()`) and `--partial-dir` on first use, and only stats them at
/// startup (main.c:1036-1054). A Landlock rule needs an existing inode, so a
/// directory that is not there yet is covered through its nearest existing
/// ancestor: the sandbox is wider for that run, and the behaviour stays
/// upstream's (user decision 2026-11-03: never pre-create the directory,
/// which upstream would not). An existing directory gets an exact rule. The
/// widening is oc-only, so it is reported only through `--debug=landlock`,
/// never on a channel upstream's output would show. The filesystem root is
/// never granted that way - that would be no sandbox at all.
fn staging_write_root(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let existing = path.ancestors().find(|ancestor| ancestor.exists())?;
    let canonical = existing.canonicalize().ok()?;
    canonical.parent()?;
    if existing != path {
        logging::debug_log!(
            Landlock,
            1,
            "landlock: {} does not exist yet; granting write on its nearest existing ancestor {}",
            path.display(),
            canonical.display()
        );
    }
    Some(canonical)
}

fn landlock_root_for_dest(dest: &std::path::Path) -> Option<std::path::PathBuf> {
    dest.canonicalize()
        .ok()
        .and_then(|canonical| {
            if canonical.is_dir() {
                Some(canonical)
            } else {
                canonical.parent().map(std::path::Path::to_path_buf)
            }
        })
        .or_else(|| dest.parent().and_then(|parent| parent.canonicalize().ok()))
}

/// Surfaces the outcome of the `--server` receiver's Landlock install through
/// the existing logging funnel; the transfer continues in every case.
///
/// Landlock is oc-only defence-in-depth (upstream rsync has no counterpart),
/// so there is no upstream message to mirror - but the routing rides the
/// funnel every other diagnostic uses. The classes, and why each sits at its
/// gate (degrade gracefully but visibly; aborting the transfer was rejected):
///
/// - **Genuine install failure** (`Error`, and `Enforced(NotEnforced)`, which
///   is an install the kernel accepted and then applied nothing of): a
///   defence layer silently vanished on a host that advertised it, so it is a
///   [`logging::warn_log!`] - upstream's `FWARNING` class, dispatched by
///   `rwrite()` (upstream: log.c:341) at every verbosity, exactly like the
///   receiver's lost-anchor warning
///   (`transfer/src/receiver/transfer/setup/sandbox.rs`).
/// - **Availability degradation** (`Enforced(PartiallyEnforced)`): the normal
///   state on 5.13-6.6 kernels, where BestEffort drops the rights the ABI
///   lacks. Warning on every transfer would be noise, so it is
///   [`logging::debug_log!`] at `Landlock` 1, the oc-only category whose
///   decision level `-vvv` implies alongside RECV (`debug_verbosity[3]`,
///   upstream: options.c:248).
/// - **Availability race** (`Unavailable` after a positive [`is_supported`]
///   probe - the stub never gets here because its probe returns `false`):
///   anomalous like a failed install, warned like one; matches the daemon
///   site's arm in `daemon/src/daemon/sections/module_access/transfer/sandbox.rs`.
/// - **Success** (`Enforced(FullyEnforced)`): debug at the same `Landlock` 1
///   gate, so `-vvv` can tell "engaged" from "skipped" without adding noise
///   at lower verbosity.
///
/// Whenever the ruleset was enforced, `Landlock` 2 (`-vvvv`, or
/// `--debug=landlock2`) adds the detail: every allowlisted path and, for a
/// partial enforcement, the rights this kernel's ABI dropped.
///
/// [`is_supported`]: fast_io::landlock::is_supported
fn surface_landlock_outcome(
    outcome: &fast_io::landlock::LandlockOutcome,
    root: &std::path::Path,
    allowed: &[&std::path::Path],
) {
    use fast_io::landlock::{EnforcementStatus, LandlockOutcome};
    let root_count = allowed.len();

    match outcome {
        LandlockOutcome::Enforced(EnforcementStatus::FullyEnforced) => {
            logging::debug_log!(
                Landlock,
                1,
                "landlock: fully enforced over {root_count} root(s)"
            );
            trace_landlock_allowlist(allowed);
        }
        LandlockOutcome::Enforced(EnforcementStatus::PartiallyEnforced) => {
            trace_landlock_allowlist(allowed);
            if let Some(dropped) = fast_io::landlock::best_effort_fs_downgrade() {
                logging::debug_log!(
                    Landlock,
                    2,
                    "landlock: rights dropped by this ABI: {dropped}"
                );
            }
            logging::debug_log!(
                Landlock,
                1,
                "landlock: partially enforced over {root_count} root(s) - this kernel's Landlock ABI lacks some requested rights; SEC-1 *at* helpers back the rest"
            );
        }
        LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => {
            logging::warn_log!(
                "landlock: ruleset accepted but NOT enforced for root {}; continuing with SEC-1 *at* helpers as the sole defense",
                root.display()
            );
        }
        LandlockOutcome::Unavailable => {
            logging::warn_log!(
                "landlock: probe positive but the kernel reported it unavailable; continuing with SEC-1 *at* helpers as the sole defense"
            );
        }
        LandlockOutcome::Error(err) => {
            logging::warn_log!(
                "landlock: setup failed for root {}: {err}; continuing with SEC-1 *at* helpers as the sole defense",
                root.display()
            );
        }
    }
}

/// Traces every path the Landlock ruleset admits, at `Landlock` 2 (`-vvvv`
/// or `--debug=landlock2`): the detail behind the level-1 decision line.
fn trace_landlock_allowlist(allowed: &[&std::path::Path]) {
    if logging::debug_gte(logging::DebugFlag::Landlock, 2) {
        let rendered: Vec<String> = allowed.iter().map(|p| p.display().to_string()).collect();
        logging::debug_log!(Landlock, 2, "landlock: allowlist: {}", rendered.join(", "));
    }
}

/// Collects the canonical directory targets of pre-existing destination
/// symlink-to-directory entries so they can be added to the `--keep-dirlinks`
/// Landlock allowlist.
///
/// Walks the real directory structure beneath `root` without descending into
/// symlinks (avoiding loops); each symlink whose target resolves to a directory
/// contributes its canonical path. The walk is bounded in depth and total
/// entries so a hostile or pathological destination tree cannot stall receiver
/// startup. Mirrors upstream's `--keep-dirlinks` trust model: the destination
/// symlinks the user pre-established are followed even when their targets sit
/// outside the transfer root (`generator.c:1745`).
fn collect_keep_dirlink_targets(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    const MAX_DEPTH: usize = 32;
    const MAX_ENTRIES: usize = 100_000;

    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut visited = 0usize;

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                return;
            }
            let path = entry.path();
            // Classify without following the symlink so loops are impossible.
            let Ok(lst) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if lst.file_type().is_symlink() {
                // A symlink that resolves to a directory is a kept dirlink;
                // allowlist its canonical target so writes through it succeed.
                if let Ok(canon) = std::fs::canonicalize(&path)
                    && std::fs::metadata(&canon).is_ok_and(|m| m.file_type().is_dir())
                    && seen.insert(canon.clone())
                {
                    out.push(canon);
                }
            } else if lst.file_type().is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
}

#[cfg(test)]
mod landlock_root_tests {
    use std::fs;

    use super::landlock_root_for_dest;

    /// The receiver's `mkstemp` targets the destination's own directory, so the
    /// allowlist root must be a directory in every shape the operand can take.
    /// The existing-file row is the regression: `canonicalize()` succeeds on the
    /// file, and a Landlock rule over a regular file grants that file alone, so
    /// returning the operand there denies the sibling temp with `EACCES`.
    #[test]
    fn root_is_the_written_directory_for_every_operand_shape() {
        let scratch = test_support::create_tempdir();
        let dir = scratch.path().join("dest");
        fs::create_dir(&dir).unwrap();
        let existing_file = dir.join("f.bin");
        fs::write(&existing_file, b"basis").unwrap();
        let absent = dir.join("not-yet.bin");

        let canonical_dir = fs::canonicalize(&dir).unwrap();

        assert_eq!(
            landlock_root_for_dest(&dir).as_ref(),
            Some(&canonical_dir),
            "a directory operand is its own root"
        );
        assert_eq!(
            landlock_root_for_dest(&existing_file).as_ref(),
            Some(&canonical_dir),
            "an existing destination file must resolve to its parent directory"
        );
        assert_eq!(
            landlock_root_for_dest(&absent).as_ref(),
            Some(&canonical_dir),
            "a not-yet-created destination keeps resolving to its parent"
        );
    }
}

#[cfg(all(test, unix))]
mod keep_dirlink_target_tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::collect_keep_dirlink_targets;

    /// A destination symlink resolving to a directory contributes its canonical
    /// target so the `--keep-dirlinks` Landlock allowlist permits writes through
    /// it; a symlink-to-file and a plain directory contribute nothing.
    #[test]
    fn collects_only_symlink_to_dir_targets() {
        let scratch = test_support::create_tempdir();
        let root = scratch.path().join("dest");
        fs::create_dir(&root).unwrap();

        // A symlink-to-directory whose target lives outside the dest root.
        let outside = scratch.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("dlink")).unwrap();

        // A symlink-to-file (not a dirlink) and a plain directory: neither is a
        // kept dirlink target.
        let file = scratch.path().join("f.txt");
        fs::write(&file, b"x").unwrap();
        symlink(&file, root.join("flink")).unwrap();
        fs::create_dir(root.join("realdir")).unwrap();

        let mut out = Vec::new();
        collect_keep_dirlink_targets(&root, &mut out);

        let canon_outside = fs::canonicalize(&outside).unwrap();
        assert!(
            out.contains(&canon_outside),
            "the symlink-to-directory target must be allowlisted: {out:?}"
        );
        assert_eq!(
            out.len(),
            1,
            "only the symlink-to-directory contributes a target: {out:?}"
        );
    }

    /// A nested symlink-to-directory (below the root) is also collected, since
    /// upstream follows kept dirlinks at any depth.
    #[test]
    fn collects_nested_symlink_to_dir_targets() {
        let scratch = test_support::create_tempdir();
        let root = scratch.path().join("dest");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let outside = scratch.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, sub.join("dlink")).unwrap();

        let mut out = Vec::new();
        collect_keep_dirlink_targets(&root, &mut out);

        let canon_outside = fs::canonicalize(&outside).unwrap();
        assert!(
            out.contains(&canon_outside),
            "a nested kept-dirlink target must be allowlisted: {out:?}"
        );
    }
}

// A failed Landlock install degrades gracefully but VISIBLY. Each
// class below asserts its message at the intended gate AND its absence below
// it, by exact event count rather than substring presence - a `contains()`
// over a joined log cannot tell one emission from two, and cannot prove the
// below-gate arm emitted nothing at all.
//
// Soundness: `logging` state is thread-local and nextest runs one process per
// test, so each test's `init` + `drain_events` cannot observe a sibling.
#[cfg(test)]
mod landlock_surfacing_tests {
    use std::io;
    use std::path::Path;

    /// A staging dir that exists gets an exact rule; a missing one is covered
    /// through its nearest existing ancestor, and the widening is named on
    /// `--debug=landlock` only.
    #[test]
    fn a_missing_staging_dir_is_granted_through_its_nearest_ancestor_and_logged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonical tempdir");
        let existing = base.join("bak");
        std::fs::create_dir(&existing).expect("mkdir bak");

        init(VerbosityConfig::default());
        logging::apply_debug_flag("landlock").expect("debug token parses");
        drain_events();
        assert_eq!(super::staging_write_root(&existing), Some(existing.clone()));
        assert_eq!(
            count_landlock_debug(&drain_events(), 1, "nearest existing ancestor"),
            0
        );

        let missing = base.join("missing/deeper");
        assert_eq!(super::staging_write_root(&missing), Some(base.clone()));
        let events = drain_events();
        assert_eq!(
            count_landlock_debug(&events, 1, &base.display().to_string()),
            1
        );

        init(VerbosityConfig::from_verbose_level(1));
        drain_events();
        assert_eq!(super::staging_write_root(&missing), Some(base));
        assert_eq!(
            count_landlock_debug(&drain_events(), 1, "nearest existing ancestor"),
            0,
            "plain -v must stay byte-identical to 3.5.1"
        );
    }

    #[test]
    fn the_filesystem_root_is_never_granted_for_a_missing_staging_dir() {
        let missing = Path::new("/oc-rsync-no-such-staging-dir/backups");
        assert!(!missing.exists());
        assert_eq!(super::staging_write_root(missing), None);
    }

    use fast_io::landlock::{EnforcementStatus, LandlockOutcome};
    use logging::{DebugFlag, DiagnosticEvent, LogCode, VerbosityConfig, drain_events, init};

    use super::surface_landlock_outcome;

    fn landlock_events(verbose_level: u8, outcome: &LandlockOutcome) -> Vec<DiagnosticEvent> {
        init(VerbosityConfig::from_verbose_level(verbose_level));
        drain_events();
        surface_landlock_outcome(outcome, Path::new("/srv/dest"), &[Path::new("/srv/dest")]);
        drain_events()
    }
    fn landlock_events_with(token: &str, outcome: &LandlockOutcome) -> Vec<DiagnosticEvent> {
        init(VerbosityConfig::default());
        logging::apply_debug_flag(token).expect("debug token parses");
        drain_events();
        surface_landlock_outcome(outcome, Path::new("/srv/dest"), &[Path::new("/srv/dest")]);
        drain_events()
    }

    fn count_warnings(events: &[DiagnosticEvent], needle: &str) -> usize {
        events
            .iter()
            .filter(|event| match event {
                DiagnosticEvent::Info { code, message, .. } => {
                    *code == LogCode::Warning && message.contains(needle)
                }
                DiagnosticEvent::Debug { .. } | DiagnosticEvent::Bytes { .. } => false,
            })
            .count()
    }

    fn count_landlock_debug(events: &[DiagnosticEvent], at: u8, needle: &str) -> usize {
        events
            .iter()
            .filter(|event| match event {
                DiagnosticEvent::Debug {
                    flag,
                    level,
                    message,
                    ..
                } => *flag == DebugFlag::Landlock && *level == at && message.contains(needle),
                DiagnosticEvent::Info { .. } | DiagnosticEvent::Bytes { .. } => false,
            })
            .count()
    }

    /// ERROR class: a genuine install failure is a warning, and a warning is
    /// not verbosity-gated - it must appear exactly once at verbosity 0, the
    /// level the pre-fix abort arm never let the operator see anything at.
    #[test]
    fn a_failed_install_warns_once_at_zero_verbosity() {
        let outcome = LandlockOutcome::Error(io::Error::other("planted failure"));
        let events = landlock_events(0, &outcome);

        assert_eq!(
            count_warnings(&events, "landlock: setup failed for root /srv/dest"),
            1,
            "a failed Landlock install must warn exactly once, got: {events:?}"
        );
        assert_eq!(
            events.len(),
            1,
            "the failure arm must emit nothing besides its warning: {events:?}"
        );
    }

    /// ERROR class, sibling producer: a ruleset the kernel accepted but did
    /// not enforce is a vanished defence layer, warned like a failed install.
    #[test]
    fn a_not_enforced_ruleset_warns_once_at_zero_verbosity() {
        let outcome = LandlockOutcome::Enforced(EnforcementStatus::NotEnforced);
        let events = landlock_events(0, &outcome);

        assert_eq!(
            count_warnings(&events, "NOT enforced for root /srv/dest"),
            1,
            "an unenforced ruleset must warn exactly once, got: {events:?}"
        );
    }

    /// ERROR-adjacent: `Unavailable` here means the probe said yes and the
    /// install then said no - an anomaly, not the normal old-kernel state
    /// (which never reaches the install; see the `!is_supported()` arm).
    #[test]
    fn an_unavailable_race_warns_once_at_zero_verbosity() {
        let events = landlock_events(0, &LandlockOutcome::Unavailable);

        assert_eq!(
            count_warnings(&events, "probe positive"),
            1,
            "the probe/install race must warn exactly once, got: {events:?}"
        );
    }

    /// AVAILABILITY class: partial enforcement is the expected state on a
    /// 5.13-6.6 kernel, so it must be silent below the debug gate and appear
    /// exactly once at it (`-vvv` sets RECV to 1; `-vv` does not).
    #[test]
    fn partial_enforcement_is_debug_gated() {
        let outcome = LandlockOutcome::Enforced(EnforcementStatus::PartiallyEnforced);

        let below = landlock_events(2, &outcome);
        assert!(
            below.is_empty(),
            "partial enforcement is availability, not failure - it must emit \
             nothing below the recv debug gate, got: {below:?}"
        );

        let at_gate = landlock_events(3, &outcome);
        assert_eq!(
            count_landlock_debug(&at_gate, 1, "partially enforced over 1 root(s)"),
            1,
            "partial enforcement must surface exactly once at -vvv, got: {at_gate:?}"
        );
        assert_eq!(
            count_warnings(&at_gate, "landlock"),
            0,
            "partial enforcement must never escalate to a warning: {at_gate:?}"
        );
    }

    /// Success control: full enforcement warns nowhere and rides the same
    /// debug gate, so the warning assertions above cannot be satisfied by an
    /// unconditional emitter.
    #[test]
    fn full_enforcement_never_warns_and_is_debug_gated() {
        let outcome = LandlockOutcome::Enforced(EnforcementStatus::FullyEnforced);

        let below = landlock_events(2, &outcome);
        assert!(
            below.is_empty(),
            "success must be silent below the recv debug gate, got: {below:?}"
        );

        let at_gate = landlock_events(3, &outcome);
        assert_eq!(
            count_landlock_debug(&at_gate, 1, "fully enforced over 1 root(s)"),
            1,
            "success must be visible at -vvv so 'engaged' and 'skipped' are \
             distinguishable, got: {at_gate:?}"
        );
        assert_eq!(
            count_warnings(&at_gate, "landlock"),
            0,
            "success must never warn: {at_gate:?}"
        );
    }
    /// The decision line is reachable without any `-v`: `--debug=landlock`
    /// alone shows it, and level 1 stops short of the allowlist detail.
    #[test]
    fn debug_landlock_shows_the_decision_but_not_the_detail() {
        let outcome = LandlockOutcome::Enforced(EnforcementStatus::FullyEnforced);
        let events = landlock_events_with("landlock", &outcome);
        assert_eq!(
            count_landlock_debug(&events, 1, "fully enforced over 1 root(s)"),
            1,
            "{events:?}"
        );
        assert_eq!(
            count_landlock_debug(&events, 2, "allowlist"),
            0,
            "{events:?}"
        );
        let seccomp_only = landlock_events_with("seccomp2", &outcome);
        assert!(
            seccomp_only.is_empty(),
            "seccomp must not enable landlock: {seccomp_only:?}"
        );
    }
    /// The allowlist detail is level 2 only: `-vvvv` and `--debug=landlock2`
    /// show it, `-vvv` does not, and the `sandbox` umbrella reaches it too.
    #[test]
    fn the_allowlist_detail_appears_only_at_level_two() {
        let outcome = LandlockOutcome::Enforced(EnforcementStatus::FullyEnforced);
        let at_three = landlock_events(3, &outcome);
        assert_eq!(
            count_landlock_debug(&at_three, 2, "allowlist"),
            0,
            "{at_three:?}"
        );
        let at_four = landlock_events(4, &outcome);
        assert_eq!(
            count_landlock_debug(&at_four, 2, "landlock: allowlist: /srv/dest"),
            1,
            "{at_four:?}"
        );
        for token in ["landlock2", "sandbox2"] {
            let events = landlock_events_with(token, &outcome);
            assert_eq!(
                count_landlock_debug(&events, 2, "landlock: allowlist: /srv/dest"),
                1,
                "{token}: {events:?}"
            );
        }
    }
    /// Level 0 is silent for every non-failure outcome.
    #[test]
    fn landlock_level_zero_is_silent() {
        for outcome in [
            LandlockOutcome::Enforced(EnforcementStatus::FullyEnforced),
            LandlockOutcome::Enforced(EnforcementStatus::PartiallyEnforced),
        ] {
            let events = landlock_events(0, &outcome);
            assert!(events.is_empty(), "{events:?}");
        }
    }
}

/// Unbuffered handles on the server's descriptors 0 and 1.
#[cfg(unix)]
struct ServerStdio {
    stdin: std::fs::File,
    stdout: std::fs::File,
}

#[cfg(unix)]
impl ServerStdio {
    fn open() -> io::Result<Self> {
        use std::os::fd::AsFd;
        Ok(Self {
            stdin: io::stdin().as_fd().try_clone_to_owned()?.into(),
            stdout: io::stdout().as_fd().try_clone_to_owned()?.into(),
        })
    }
}
