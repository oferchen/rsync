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
