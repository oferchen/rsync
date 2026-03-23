//! Write strategy selection for file transfers.
//!
//! Determines how a file is written to disk based on transfer flags and
//! destination state. Mirrors upstream `receiver.c` logic which selects among
//! five paths: append, inplace, direct, anonymous temp file, or named temp
//! file with atomic rename.

use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use logging::debug_log;

use crate::local_copy::{CopyContext, LocalCopyError};

use super::super::super::guard::DestinationWriteGuard;

/// The write strategy for transferring a file to disk.
///
/// Mirrors upstream `receiver.c` logic which selects among five paths based on
/// transfer mode flags and destination state. The strategy is determined purely
/// from flags - no I/O - then executed by `open_destination_writer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::local_copy) enum WriteStrategy {
    /// Open existing file and seek to append offset.
    Append,
    /// Write directly to destination without temp file.
    /// Truncates when no delta signature exists.
    Inplace,
    /// Create new file directly - no existing destination to protect.
    /// Uses `create_new(true)` to prevent races with concurrent writers.
    Direct,
    /// Create a staging temp file then rename atomically.
    /// Used when an existing destination must be protected, or when
    /// `--partial`, `--delay-updates`, or `--temp-dir` is active.
    TempFileRename,
    /// Use Linux `O_TMPFILE` to create an anonymous inode, then `linkat(2)`
    /// to materialize it at the destination. Falls back to `TempFileRename`
    /// if `O_TMPFILE` is not available at runtime.
    AnonymousTempFile,
}

/// Determines the write strategy from transfer flags and destination state.
///
/// This is a pure function with no I/O - it only inspects flags to decide
/// which strategy `open_destination_writer` should execute.
///
/// # Strategy selection (upstream: receiver.c)
///
/// 1. **Append** - `append_offset > 0`: resume writing at end of existing file.
/// 2. **Inplace** - `--inplace`: write directly, truncating only when no delta.
/// 3. **Direct** - no existing destination AND none of `--partial`,
///    `--delay-updates`, `--temp-dir`: create file directly.
/// 4. **AnonymousTempFile** - Linux with `O_TMPFILE` support, no `--partial`
///    (partial files need a visible staging path for resume), no `--temp-dir`
///    (cross-device linkat would fail): anonymous inode + `linkat(2)`.
/// 5. **TempFileRename** - all other cases: temp file + atomic rename.
pub(in crate::local_copy) fn select_write_strategy(
    append_offset: u64,
    inplace_enabled: bool,
    partial_enabled: bool,
    delay_updates_enabled: bool,
    has_existing_destination: bool,
    has_temp_directory: bool,
    destination: &Path,
) -> WriteStrategy {
    if append_offset > 0 {
        WriteStrategy::Append
    } else if inplace_enabled {
        WriteStrategy::Inplace
    } else if !has_existing_destination
        && !partial_enabled
        && !delay_updates_enabled
        && !has_temp_directory
    {
        WriteStrategy::Direct
    } else if !partial_enabled && !has_temp_directory && can_use_anonymous_tmpfile(destination) {
        WriteStrategy::AnonymousTempFile
    } else {
        WriteStrategy::TempFileRename
    }
}

/// Returns `true` if anonymous `O_TMPFILE` is available for the destination's filesystem.
///
/// On Linux, probes the destination's parent directory. On other platforms, always
/// returns `false`.
pub(in crate::local_copy) fn can_use_anonymous_tmpfile(destination: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let dir = destination.parent().unwrap_or(Path::new("."));
        fast_io::o_tmpfile_available(dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = destination;
        false
    }
}

/// Opens the destination file using the pre-selected write strategy.
///
/// Each strategy maps to a distinct I/O path:
/// - **Append**: opens existing file and seeks to append offset
/// - **Inplace**: opens for writing without temp file (truncates only when no delta)
/// - **Direct**: creates new file directly when no existing destination
/// - **TempFileRename**: creates a staging file via `DestinationWriteGuard`
#[allow(clippy::too_many_arguments)]
pub(in crate::local_copy) fn open_destination_writer(
    context: &CopyContext,
    destination: &Path,
    record_path: &Path,
    delta_signature: &Option<crate::delta::DeltaSignatureIndex>,
    append_offset: u64,
    partial_enabled: bool,
    strategy: WriteStrategy,
    guard: &mut Option<DestinationWriteGuard>,
    staging_path: &mut Option<PathBuf>,
) -> Result<fs::File, LocalCopyError> {
    match strategy {
        WriteStrategy::Append => {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(destination)
                .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
            file.seek(SeekFrom::Start(append_offset))
                .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
            Ok(file)
        }
        WriteStrategy::Inplace => {
            // For inplace with delta, do NOT truncate - we read existing blocks
            let should_truncate = delta_signature.is_none();
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(should_truncate)
                .open(destination)
                .map_err(|error| LocalCopyError::io("copy file", destination, error))
        }
        WriteStrategy::Direct => {
            // upstream: receiver.c - direct write when no existing file to protect.
            // create_new(true) prevents races with concurrent writers (EEXIST).
            debug_log!(
                Io,
                3,
                "direct write to {} (no existing destination)",
                record_path.display()
            );
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination)
                .map_err(|error| LocalCopyError::io("copy file", destination, error))
        }
        WriteStrategy::AnonymousTempFile => {
            #[cfg(target_os = "linux")]
            {
                match DestinationWriteGuard::new_anonymous(destination) {
                    Ok((new_guard, file)) => {
                        debug_log!(
                            Io,
                            3,
                            "opened anonymous temp file (O_TMPFILE) for {}",
                            record_path.display()
                        );
                        *guard = Some(new_guard);
                        return Ok(file);
                    }
                    Err(_) => {
                        // O_TMPFILE failed at open time (race with probe, or fd exhaustion).
                        // Fall through to named temp file.
                        debug_log!(
                            Io,
                            3,
                            "O_TMPFILE open failed, falling back to named temp file for {}",
                            record_path.display()
                        );
                    }
                }
            }
            // Fallback: named temp file (also the only path on non-Linux).
            let (new_guard, file) = DestinationWriteGuard::new(
                destination,
                partial_enabled,
                context.partial_directory_path(),
                context.temp_directory_path(),
            )?;
            *staging_path = Some(new_guard.staging_path().to_path_buf());
            debug_log!(
                Io,
                3,
                "created temp file {} for {}",
                new_guard.staging_path().display(),
                record_path.display()
            );
            *guard = Some(new_guard);
            Ok(file)
        }
        WriteStrategy::TempFileRename => {
            let (new_guard, file) = DestinationWriteGuard::new(
                destination,
                partial_enabled,
                context.partial_directory_path(),
                context.temp_directory_path(),
            )?;
            *staging_path = Some(new_guard.staging_path().to_path_buf());
            debug_log!(
                Io,
                3,
                "created temp file {} for {}",
                new_guard.staging_path().display(),
                record_path.display()
            );
            *guard = Some(new_guard);
            Ok(file)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nonexistent path where `O_TMPFILE` is guaranteed unavailable, so
    /// strategy selection falls through to `TempFileRename` on all platforms.
    const NO_TMPFILE: &str = "/nonexistent_o_tmpfile_strategy_test";

    /// Helper that calls `select_write_strategy` with a path where `O_TMPFILE`
    /// is unavailable, preserving the existing test semantics.
    fn strategy(
        append_offset: u64,
        inplace: bool,
        partial: bool,
        delay: bool,
        existing: bool,
        temp_dir: bool,
    ) -> WriteStrategy {
        select_write_strategy(
            append_offset,
            inplace,
            partial,
            delay,
            existing,
            temp_dir,
            Path::new(NO_TMPFILE),
        )
    }

    // --- Append strategy ---

    #[test]
    fn append_offset_selects_append_strategy() {
        assert_eq!(
            strategy(1024, false, false, false, false, false),
            WriteStrategy::Append
        );
    }

    #[test]
    fn append_offset_overrides_inplace() {
        assert_eq!(
            strategy(512, true, false, false, true, false),
            WriteStrategy::Append
        );
    }

    #[test]
    fn append_offset_overrides_partial() {
        assert_eq!(
            strategy(256, false, true, false, false, false),
            WriteStrategy::Append
        );
    }

    // --- Inplace strategy ---

    #[test]
    fn inplace_enabled_selects_inplace_strategy() {
        assert_eq!(
            strategy(0, true, false, false, true, false),
            WriteStrategy::Inplace
        );
    }

    #[test]
    fn inplace_without_existing_dest_still_selects_inplace() {
        assert_eq!(
            strategy(0, true, false, false, false, false),
            WriteStrategy::Inplace
        );
    }

    #[test]
    fn inplace_overrides_partial_and_delay_updates() {
        assert_eq!(
            strategy(0, true, true, true, true, true),
            WriteStrategy::Inplace
        );
    }

    // --- Direct strategy ---

    #[test]
    fn no_existing_dest_selects_direct_strategy() {
        assert_eq!(
            strategy(0, false, false, false, false, false),
            WriteStrategy::Direct
        );
    }

    // --- TempFileRename strategy ---

    #[test]
    fn partial_forces_temp_file_rename() {
        assert_eq!(
            strategy(0, false, true, false, false, false),
            WriteStrategy::TempFileRename
        );
    }

    #[test]
    fn delay_updates_forces_temp_file_rename() {
        assert_eq!(
            strategy(0, false, false, true, false, false),
            WriteStrategy::TempFileRename
        );
    }

    #[test]
    fn temp_dir_forces_temp_file_rename() {
        assert_eq!(
            strategy(0, false, false, false, false, true),
            WriteStrategy::TempFileRename
        );
    }

    #[test]
    fn existing_dest_forces_temp_file_rename() {
        assert_eq!(
            strategy(0, false, false, false, true, false),
            WriteStrategy::TempFileRename
        );
    }

    #[test]
    fn existing_dest_with_partial_forces_temp_file_rename() {
        assert_eq!(
            strategy(0, false, true, false, true, false),
            WriteStrategy::TempFileRename
        );
    }

    #[test]
    fn all_temp_file_flags_active_selects_temp_file_rename() {
        assert_eq!(
            strategy(0, false, true, true, true, true),
            WriteStrategy::TempFileRename
        );
    }

    // --- Priority ordering ---

    #[test]
    fn append_has_highest_priority() {
        assert_eq!(
            strategy(100, true, true, true, true, true),
            WriteStrategy::Append
        );
    }

    #[test]
    fn inplace_has_second_highest_priority() {
        assert_eq!(
            strategy(0, true, true, true, true, true),
            WriteStrategy::Inplace
        );
    }

    #[test]
    fn direct_requires_all_conditions_false() {
        assert_eq!(
            strategy(0, false, true, false, false, false),
            WriteStrategy::TempFileRename
        );
        assert_eq!(
            strategy(0, false, false, true, false, false),
            WriteStrategy::TempFileRename
        );
        assert_eq!(
            strategy(0, false, false, false, true, false),
            WriteStrategy::TempFileRename
        );
        assert_eq!(
            strategy(0, false, false, false, false, true),
            WriteStrategy::TempFileRename
        );
        assert_eq!(
            strategy(0, false, false, false, false, false),
            WriteStrategy::Direct
        );
    }

    // --- AnonymousTempFile strategy ---

    #[test]
    fn partial_prevents_anonymous_strategy() {
        // Even on a real tmpdir, partial forces TempFileRename because partial
        // files need a visible staging path.
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        let result = select_write_strategy(0, false, true, false, true, false, &dest);
        assert_eq!(result, WriteStrategy::TempFileRename);
    }

    #[test]
    fn temp_dir_prevents_anonymous_strategy() {
        // --temp-dir prevents anonymous because linkat cannot cross devices.
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        let result = select_write_strategy(0, false, false, false, true, true, &dest);
        assert_eq!(result, WriteStrategy::TempFileRename);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn anonymous_selected_when_o_tmpfile_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        if !can_use_anonymous_tmpfile(&dest) {
            // O_TMPFILE not supported on this fs; skip.
            return;
        }
        // With existing dest, no partial, no temp-dir -> should pick anonymous.
        let result = select_write_strategy(0, false, false, false, true, false, &dest);
        assert_eq!(result, WriteStrategy::AnonymousTempFile);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn anonymous_with_delay_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        if !can_use_anonymous_tmpfile(&dest) {
            return;
        }
        // delay_updates alone should still allow anonymous.
        let result = select_write_strategy(0, false, false, true, true, false, &dest);
        assert_eq!(result, WriteStrategy::AnonymousTempFile);
    }

    #[test]
    fn can_use_anonymous_returns_false_for_nonexistent_dir() {
        assert!(!can_use_anonymous_tmpfile(Path::new(
            "/no_such_dir_tmpfile_test/file"
        )));
    }
}
