//! Secrets file permission validation.
//!
//! # Unix
//!
//! Checks that secrets files are not other-accessible and (when running as
//! root) are owned by root.
//!
//! # Other
//!
//! No-op - always succeeds.
//!
//! # Upstream Reference
//!
//! `authenticate.c:check_secret()` - permission checks for secrets files.

use std::io;
use std::path::Path;

/// Checks that a secrets file has appropriately restrictive permissions.
///
/// On Unix, verifies the file is not other-accessible (`mode & 0o006`).
/// When the daemon runs as root, also verifies the file is owned by root.
///
/// upstream: authenticate.c:172-179 `check_secret()` - `(st.st_mode & 06) != 0`
/// rejects other-accessible files; `st.st_uid != ROOT_UID` rejects non-root-owned
/// files when running as root. Upstream applies both only under `strict modes`;
/// that gate is the caller's responsibility here.
#[cfg(unix)]
pub fn check_secrets_file_permissions(path: &Path) -> io::Result<()> {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::metadata(path)?;
    // upstream: rsync.h:1457 - MY_UID() is geteuid(), so a setuid daemon is
    // judged by the identity it acts with, not the one that launched it.
    check_secrets_metadata(
        metadata.permissions().mode(),
        metadata.uid(),
        nix::unistd::geteuid().is_root(),
        path,
    )
}

/// Applies upstream's two strict-modes rules to an already-fetched mode and
/// owner, so both rules are testable without running as root.
///
/// upstream: authenticate.c:172-177 check_secret() - `(st.st_mode & 06) != 0`
/// is refused first; otherwise `MY_UID() == ROOT_UID && st.st_uid != ROOT_UID`.
#[cfg(unix)]
fn check_secrets_metadata(
    mode: u32,
    owner_uid: u32,
    running_as_root: bool,
    path: &Path,
) -> io::Result<()> {
    if (mode & 0o006) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "secrets file must not be other-accessible (see strict modes option): '{}'",
                path.display()
            ),
        ));
    }

    if running_as_root && owner_uid != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "secrets file must be owned by root when running as root (see strict modes option): '{}'",
                path.display()
            ),
        ));
    }

    Ok(())
}

/// No-op permission check on non-Unix platforms.
#[cfg(not(unix))]
pub fn check_secrets_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn nonexistent_file_returns_error() {
        let result = check_secrets_file_permissions(Path::new("/nonexistent_secrets_xyz_99999"));
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn valid_permissions_succeeds() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets");
        std::fs::write(&path, "user:pass\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_secrets_file_permissions(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn other_readable_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets");
        std::fs::write(&path, "user:pass\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o604)).unwrap();
        let result = check_secrets_file_permissions(&path);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("other-accessible"));
    }

    #[cfg(unix)]
    #[test]
    fn other_writable_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets");
        std::fs::write(&path, "user:pass\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o602)).unwrap();
        let result = check_secrets_file_permissions(&path);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_without_other_access_succeeds() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets");
        std::fs::write(&path, "user:pass\n").unwrap();
        // Mode 0o640: owner rw, group r, other none - upstream allows this
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(check_secrets_file_permissions(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn other_readable_and_writable_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets");
        std::fs::write(&path, "user:pass\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o606)).unwrap();
        let result = check_secrets_file_permissions(&path);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn mode_0o600_is_valid() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets");
        std::fs::write(&path, "user:pass\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_secrets_file_permissions(&path).is_ok());
    }

    /// upstream: authenticate.c:175-177 - a root daemon refuses a secrets file
    /// that root does not own, however tight its mode.
    #[cfg(unix)]
    #[test]
    fn root_refuses_a_secrets_file_root_does_not_own() {
        let err = check_secrets_metadata(0o600, 1000, true, Path::new("secrets"))
            .expect_err("non-root owner must be refused when running as root");
        assert!(err.to_string().contains("must be owned by root"));
        assert!(check_secrets_metadata(0o600, 0, true, Path::new("secrets")).is_ok());
    }

    /// The ownership rule applies only to a root daemon.
    #[cfg(unix)]
    #[test]
    fn non_root_accepts_a_secrets_file_it_owns() {
        assert!(check_secrets_metadata(0o600, 1000, false, Path::new("secrets")).is_ok());
    }

    /// upstream: authenticate.c:172 - the mode rule is checked first and does
    /// not depend on who runs the daemon.
    #[cfg(unix)]
    #[test]
    fn mode_rule_precedes_the_ownership_rule() {
        let err = check_secrets_metadata(0o604, 1000, true, Path::new("secrets"))
            .expect_err("other-readable must be refused");
        assert!(err.to_string().contains("other-accessible"));
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_always_succeeds() {
        let result = check_secrets_file_permissions(Path::new("C:\\nonexistent_secrets_xyz_99999"));
        assert!(result.is_ok());
    }
}
