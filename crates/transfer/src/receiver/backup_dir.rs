//! The receiver's `--backup-dir` pre-flight.
//!
//! upstream: main.c:1036-1057 do_recv() stats the backup directory once,
//! after get_local_name() has entered the destination and before any file
//! is transferred, so a relative value names a directory under the
//! destination.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// A `--backup-dir` the receiver refuses before transferring anything.
#[derive(Debug)]
pub enum BackupDirRefusal {
    /// The path exists but is not a directory.
    /// upstream: main.c:1043-1045 - `RERR_SYNTAX`.
    NotDirectory(String),
    /// `stat` failed for a reason other than the path's absence.
    /// upstream: main.c:1047-1049 - `RERR_FILEIO`.
    Stat(String, io::Error),
}

impl fmt::Display for BackupDirRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDirectory(shown) => write!(f, "The backup-dir is not a directory: {shown}"),
            Self::Stat(shown, cause) => write!(
                f,
                "Failed to stat {shown}: {}",
                logging::upstream_strerror(cause)
            ),
        }
    }
}

impl std::error::Error for BackupDirRefusal {}

/// Stats `backup_dir` the way upstream's do_recv() does, resolving a relative
/// value against `dest_dir`.
///
/// A missing directory is accepted: upstream only notes it as `(new)` and
/// creates it on the first backup. A value that names the destination itself
/// (`.`) disables the backup directory upstream (options.c:2516-2527), so it
/// is not checked.
pub fn check_backup_dir(backup_dir: &Path, dest_dir: &Path) -> Result<(), BackupDirRefusal> {
    let Some(shown) = shown_backup_dir(backup_dir) else {
        return Ok(());
    };
    // Collecting the components drops the trailing slash upstream lops at
    // main.c:1039-1040, so a regular file is reported as one rather than as
    // `ENOTDIR`.
    let normalized: PathBuf = backup_dir.components().collect();
    let target = if normalized.has_root() {
        normalized
    } else {
        dest_dir.join(normalized)
    };
    match std::fs::metadata(&target) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(BackupDirRefusal::NotDirectory(shown)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(BackupDirRefusal::Stat(shown, err)),
    }
}

/// The value as upstream's `backup_dir_buf` names it in the diagnostic, or
/// `None` when upstream drops the backup directory.
///
/// upstream: options.c:2514-2530 strips leading `./` pairs and a lone `.`,
/// then appends a slash; main.c:1039-1040 removes the last byte again when the
/// buffer is longer than one byte.
fn shown_backup_dir(backup_dir: &Path) -> Option<String> {
    let given = backup_dir.to_string_lossy();
    let mut rest = given.as_ref();
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    if rest.is_empty() || rest == "." {
        return None;
    }
    let shown = match rest.strip_suffix('/') {
        Some(trimmed) if !trimmed.is_empty() => trimmed,
        _ => rest,
    };
    Some(shown.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_value_drops_leading_dot_slash_and_one_trailing_slash() {
        assert_eq!(
            shown_backup_dir(Path::new("./././bak//")).as_deref(),
            Some("bak/")
        );
        assert_eq!(shown_backup_dir(Path::new("/")).as_deref(), Some("/"));
        assert_eq!(
            shown_backup_dir(Path::new("/srv/bak")).as_deref(),
            Some("/srv/bak")
        );
        assert_eq!(shown_backup_dir(Path::new("./.")), None);
        assert_eq!(shown_backup_dir(Path::new("")), None);
    }

    #[test]
    fn missing_backup_dir_is_accepted_because_the_first_backup_creates_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(check_backup_dir(Path::new("new/deeper"), temp.path()).is_ok());
    }

    #[test]
    fn regular_file_is_refused_as_not_a_directory_even_with_a_trailing_slash() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("reg"), b"x").expect("write");
        let refusal = check_backup_dir(Path::new("./reg/"), temp.path()).expect_err("file");
        assert_eq!(
            refusal.to_string(),
            "The backup-dir is not a directory: reg"
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_below_a_regular_file_is_a_stat_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("reg"), b"x").expect("write");
        let refusal = check_backup_dir(Path::new("reg/sub"), temp.path()).expect_err("enotdir");
        assert!(matches!(refusal, BackupDirRefusal::Stat(..)), "{refusal:?}");
        assert!(
            refusal.to_string().starts_with("Failed to stat reg/sub: "),
            "{refusal}"
        );
    }

    #[test]
    fn relative_value_resolves_against_the_destination_not_the_cwd() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dest = temp.path().join("dest");
        std::fs::create_dir_all(dest.join("bak")).expect("mkdir");
        assert!(check_backup_dir(Path::new("bak"), &dest).is_ok());
        std::fs::write(temp.path().join("bak2"), b"x").expect("write");
        assert!(check_backup_dir(Path::new("bak2"), &dest).is_ok());
    }
}
