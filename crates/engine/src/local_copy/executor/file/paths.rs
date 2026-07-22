//! Temporary, partial, and destination path computation.
//!
//! Generates temp file paths (`.<name>.XXXXXX`, upstream `get_tmpname()`),
//! partial transfer paths (`--partial-dir`), and resolves the final destination
//! for each file.

// upstream: receiver.c - temp file naming, util1.c:partial_dir_fname()

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::LocalCopyError;

/// Computes the partial-file path by prefixing the filename with `.rsync-partial-`.
pub(crate) fn partial_destination_path(destination: &Path) -> PathBuf {
    let file_name = destination.file_name().map_or_else(
        || "partial".to_owned(),
        |name| name.to_string_lossy().to_string(),
    );
    let partial_name = format!(".rsync-partial-{file_name}");
    destination.with_file_name(partial_name)
}

/// Computes the partial-file path inside a `--partial-dir` directory,
/// creating the directory if it does not exist.
pub(crate) fn partial_directory_destination_path(
    destination: &Path,
    partial_dir: &Path,
) -> Result<PathBuf, LocalCopyError> {
    let base_dir = if partial_dir.is_absolute() {
        partial_dir.to_path_buf()
    } else {
        let parent = destination
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        parent.join(partial_dir)
    };
    fs::create_dir_all(&base_dir)
        .map_err(|error| LocalCopyError::io("create partial directory", base_dir.clone(), error))?;
    let file_name = destination.file_name().map_or_else(
        || OsStr::new("partial").to_os_string(),
        |name| name.to_os_string(),
    );
    Ok(base_dir.join(file_name))
}

/// Maximum length of a single path component, minus the room reserved for the
/// leading dot and the `.XXXXXX` suffix. upstream: `NAME_MAX - 1 - TMPNAME_SUFFIX_LEN`
/// in `receiver.c:get_tmpname()` (`NAME_MAX` is 255, `TMPNAME_SUFFIX` is `.XXXXXX`).
const NAME_MAX: usize = 255;
/// Length of the `.XXXXXX` suffix (`.` plus six random characters).
const TMPNAME_SUFFIX_LEN: usize = 7;

/// Builds an rsync-style temp filename `.<base>.<suffix>` for `destination`.
///
/// Mirrors upstream `receiver.c:get_tmpname()`:
/// - the temp lives in `temp_dir` when given, otherwise in the destination's
///   own directory (so the later rename is same-filesystem/atomic);
/// - without `temp_dir` the name gets a leading dot, and a base that already
///   starts with a dot has that dot elided to avoid a double dot (OS X's sake);
/// - the base component is truncated so the full name stays within `NAME_MAX`.
///
/// `suffix` is the six-character unique component (upstream fills it via
/// `mkstemp`; the caller supplies a candidate and retries on collision).
// upstream: receiver.c:145 get_tmpname()
pub(crate) fn temp_name_with_suffix(
    destination: &Path,
    temp_dir: Option<&Path>,
    suffix: &str,
) -> PathBuf {
    let base = destination
        .file_name()
        .map_or_else(|| "".to_owned(), |name| name.to_string_lossy().to_string());
    let use_leading_dot = temp_dir.is_none();
    // Elide a base's own leading dot only when we add our own (no temp_dir).
    let trimmed_base: &str = if use_leading_dot {
        base.strip_prefix('.').unwrap_or(&base)
    } else {
        &base
    };
    let max_base = NAME_MAX
        .saturating_sub(usize::from(use_leading_dot))
        .saturating_sub(TMPNAME_SUFFIX_LEN);
    let capped: String = trimmed_base.chars().take(max_base).collect();
    let name = if use_leading_dot {
        format!(".{capped}.{suffix}")
    } else {
        format!("{capped}.{suffix}")
    };
    match temp_dir {
        Some(dir) => dir.join(name),
        None => destination.with_file_name(name),
    }
}

/// Resolves the partial-dir path for `destination` without creating the
/// directory. Absolute `partial_dir` holds partials by basename in a reserved
/// location; a relative one is resolved inside the destination file's own
/// directory. upstream: `util1.c:partial_dir_fname()`.
pub(crate) fn partial_dir_fname(destination: &Path, partial_dir: &Path) -> PathBuf {
    let base_dir = if partial_dir.is_absolute() {
        partial_dir.to_path_buf()
    } else {
        let parent = destination
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        parent.join(partial_dir)
    };
    let file_name = destination
        .file_name()
        .map_or_else(|| OsStr::new("partial").to_os_string(), OsStr::to_os_string);
    base_dir.join(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_destination_path_adds_prefix() {
        let dest = Path::new("/path/to/file.txt");
        let partial = partial_destination_path(dest);
        assert!(partial.to_string_lossy().contains(".rsync-partial-"));
        assert!(partial.to_string_lossy().contains("file.txt"));
    }

    #[test]
    fn partial_destination_path_preserves_directory() {
        let dest = Path::new("/path/to/file.txt");
        let partial = partial_destination_path(dest);
        assert_eq!(partial.parent(), dest.parent());
    }

    #[test]
    fn partial_destination_path_handles_no_filename() {
        let dest = Path::new("/");
        let partial = partial_destination_path(dest);
        assert!(partial.to_string_lossy().contains("partial"));
    }

    #[test]
    fn temp_name_with_suffix_uses_upstream_naming() {
        let dest = Path::new("/path/to/file.txt");
        let temp = temp_name_with_suffix(dest, None, "ABCDEF");
        assert_eq!(
            temp.file_name().unwrap().to_string_lossy(),
            ".file.txt.ABCDEF"
        );
        assert_eq!(temp.parent(), dest.parent());
    }

    #[test]
    fn temp_name_with_suffix_uses_temp_dir() {
        let dest = Path::new("/path/to/file.txt");
        let temp_dir = Path::new("/tmp/rsync");
        let temp = temp_name_with_suffix(dest, Some(temp_dir), "ABCDEF");
        assert!(temp.starts_with(temp_dir));
        // In --temp-dir the name has no leading dot (upstream get_tmpname()).
        assert_eq!(
            temp.file_name().unwrap().to_string_lossy(),
            "file.txt.ABCDEF"
        );
    }

    #[test]
    fn temp_name_with_suffix_elides_leading_dot() {
        let dest = Path::new("/path/to/.hidden");
        let temp = temp_name_with_suffix(dest, None, "ABCDEF");
        // A base that already starts with a dot has it elided (no double dot).
        assert_eq!(
            temp.file_name().unwrap().to_string_lossy(),
            ".hidden.ABCDEF"
        );
    }
}
