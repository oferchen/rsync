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
//! `authenticate.c` - permission checks for secrets files.

use std::io;
use std::path::Path;

/// Checks that a secrets file has appropriately restrictive permissions.
///
/// On Unix, verifies the file is not other-accessible (`mode & 0o006`).
/// When the daemon runs as root, also verifies the file is owned by root.
///
/// upstream: authenticate.c - `(st.st_mode & 06) != 0` rejects other-accessible
/// files; `st.st_uid != ROOT_UID` rejects non-root-owned files when running as root.
#[cfg(unix)]
pub fn check_secrets_file_permissions(path: &Path) -> io::Result<()> {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::metadata(path)?;
    let mode = metadata.permissions().mode();

    // upstream: authenticate.c - reject if other-readable or other-writable
    if (mode & 0o006) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "secrets file must not be other-accessible (see strict modes option): '{}'",
                path.display()
            ),
        ));
    }

    // upstream: authenticate.c - when running as root, secrets must be owned by root
    let my_uid = nix::unistd::getuid();
    if my_uid.is_root() && metadata.uid() != 0 {
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

/// No-op permission check on non-Unix platforms (matching upstream rsync).
#[cfg(not(unix))]
pub fn check_secrets_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(not(unix))]
    #[test]
    fn non_unix_always_succeeds() {
        let result =
            check_secrets_file_permissions(Path::new("C:\\nonexistent_secrets_xyz_99999"));
        assert!(result.is_ok());
    }
}
