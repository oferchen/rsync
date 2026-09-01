//! Write strategy selection for file transfers.
//!
//! Determines how a file is written to disk based on transfer flags and
//! destination state. Mirrors upstream `receiver.c` logic which selects among
//! five paths: append, inplace, direct, anonymous temp file, or named temp
//! file with atomic rename.

use std::fs;
use std::io;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use logging::debug_log;

use crate::local_copy::{CopyContext, LocalCopyError};

use super::super::super::guard::DestinationWriteGuard;
use super::super::super::paths::partial_dir_fname;

/// upstream's `partialptr` when it names an existing regular file, i.e. the
/// staging target of a `one_inplace` update.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/generator.c:2173-2179` - `partial_dir && (partialptr =
///   partial_dir_fname(fname)) != NULL && link_stat(partialptr, &partial_st, 0)
///   == 0 && S_ISREG(partial_st.st_mode)`, otherwise `partialptr = NULL`.
///   `link_stat(..., 0)` is an `lstat`, so a symlink planted at the partial leaf
///   is not a regular file and is refused here exactly as it is there.
/// - `rsync-3.5.0/receiver.c:1137-1138` - `one_inplace = inplace_partial &&
///   fnamecmp_type == FNAMECMP_PARTIAL_DIR && fd1 != -1`.
///
/// The `inplace_partial` half of that conjunction is the protocol-30
/// `CF_INPLACE_PARTIAL_DIR` capability (`compat.c:738`, `options.c:3221`), which
/// upstream negotiates with itself on a local transfer too - and this executor
/// IS both ends, so it is unconditionally true here. The `fd1 != -1` half guards
/// against a *peer* claiming a partial basis the receiver's confined open then
/// rejects (`receiver.c:1093-1105`); nothing here is peer-supplied, the path is
/// derived locally from the operator's own `--partial-dir`, so the remaining
/// condition is upstream's `partialptr != NULL` alone.
pub(in crate::local_copy) fn one_inplace_partial_file(
    context: &CopyContext,
    destination: &Path,
) -> Option<PathBuf> {
    let dir = context.partial_directory_path()?;
    let candidate = partial_dir_fname(destination, dir);
    let metadata = fs::symlink_metadata(&candidate).ok()?;
    metadata.is_file().then_some(candidate)
}

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
    /// upstream's `one_inplace`: write straight into the `--partial-dir` entry
    /// and rename that entry onto the destination on commit.
    ///
    /// upstream: `receiver.c:1195-1196` - `fnametmp = one_inplace ? partialptr
    /// : fname`. The in-place target is the file inside the partial dir, never
    /// the live destination.
    InplacePartialDir,
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
/// 2. **InplacePartialDir** - upstream's `one_inplace`: a `--partial-dir` entry
///    for this file already exists as a regular file, so the reconstruction is
///    written into it and that entry is renamed onto the destination.
/// 3. **Inplace** - `--inplace`: write directly, truncating only when no delta.
/// 4. **AnonymousTempFile** - Linux with `O_TMPFILE` support, no `--partial`
///    (partial files need a visible staging path for resume), no `--temp-dir`
///    (cross-device linkat would fail): anonymous inode + `linkat(2)`. Preferred
///    over both Direct and TempFileRename because the kernel auto-cleans the
///    anonymous inode on crash - no orphaned temp files or partial writes.
/// 5. **Direct** - no existing destination AND none of `--partial`,
///    `--delay-updates`, `--temp-dir`: create file directly.
/// 6. **TempFileRename** - all other cases: temp file + atomic rename.
///
/// `one_inplace_partial_dir` outranks plain `--inplace` for the same reason
/// upstream's `fnametmp = one_inplace ? partialptr : fname`
/// (`receiver.c:1196`) resolves the ternary before consulting `inplace`: when a
/// partial-dir entry is the update target it IS the target, whichever other
/// in-place mode is also on. `--inplace`/`--append` and `--partial-dir` are in
/// fact rejected together during config validation
/// (`core/src/client/config/builder`, upstream `options.c:2424-2432`), so the
/// precedence is not observable through the CLI; it is written this way so it
/// stays upstream's if the two are ever wired together.
///
/// ⚠ **Append is the one exception, and it is deliberate.** Upstream's ternary
/// picks the *target*; the append seek (`receiver.c:372-373`) is a separate
/// decision that upstream applies to whichever fd it opened, and its offset
/// comes from `sx.st`, which the generator has already replaced with
/// `partial_st` (`generator.c:2271`) - i.e. from the PARTIAL file's length. This
/// executor derives `append_offset` from the destination's length instead, which
/// is not the file `one_inplace` writes into, so honouring it here would seek to
/// the wrong offset in the wrong file. Append keeps its own strategy until that
/// offset is derived from the staging target.
pub(in crate::local_copy) fn select_write_strategy(
    append_offset: u64,
    inplace_enabled: bool,
    partial_enabled: bool,
    delay_updates_enabled: bool,
    has_existing_destination: bool,
    has_temp_directory: bool,
    one_inplace_partial_dir: bool,
    destination: &Path,
) -> WriteStrategy {
    if append_offset > 0 {
        WriteStrategy::Append
    } else if one_inplace_partial_dir {
        WriteStrategy::InplacePartialDir
    } else if inplace_enabled {
        WriteStrategy::Inplace
    } else if !partial_enabled && !has_temp_directory && can_use_anonymous_tmpfile(destination) {
        // O_TMPFILE preferred on Linux: atomic appearance via linkat(2) with
        // kernel auto-cleanup on crash. Avoids orphaned temp files (vs
        // TempFileRename) and partial writes visible to readers (vs Direct).
        WriteStrategy::AnonymousTempFile
    } else if !has_existing_destination
        && !partial_enabled
        && !delay_updates_enabled
        && !has_temp_directory
    {
        WriteStrategy::Direct
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
/// - **InplacePartialDir**: opens the `--partial-dir` entry through the
///   ownership walk (upstream's `one_inplace`); the commit renames it onto the
///   destination
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
    one_inplace_partial_file: Option<&Path>,
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
            // Inplace + delta must NOT truncate: the existing blocks are the
            // basis the delta reads from.
            let should_truncate = delta_signature.is_none();
            // upstream: receiver.c:1195-1224 - the whole three-arm chain, owned
            // by `fast_io`. `Direct` because this is the destination leaf,
            // already anchored by the caller, not an operator path.
            fast_io::open_inplace_output(
                destination,
                should_truncate,
                fast_io::InplaceResolution::Direct,
            )
            .map_err(|error| LocalCopyError::io("copy file", destination, error))
        }
        WriteStrategy::InplacePartialDir => {
            // upstream: receiver.c:1196 - `fnametmp = one_inplace ? partialptr
            // : fname`. The reconstruction goes into the partial-dir entry; the
            // guard's commit rename is upstream's
            // `finish_transfer(fname, fnametmp, ...)` (receiver.c:1288).
            let Some(partial_file) = one_inplace_partial_file else {
                return Err(LocalCopyError::io(
                    "copy file",
                    destination,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "one_inplace staging selected without a --partial-dir entry",
                    ),
                ));
            };
            let Some(partial_dir) = context.partial_directory_path() else {
                return Err(LocalCopyError::io(
                    "copy file",
                    destination,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "one_inplace staging selected without --partial-dir",
                    ),
                ));
            };
            // ⚠ Upstream never passes `O_TRUNC` here, and this does. The
            // difference is one unported half, not a policy choice: upstream's
            // generator makes `partialptr` the delta basis as well as the
            // in-place target (`generator.c:2270-2273` - `fnamecmp = partialptr;
            // fnamecmp_type = FNAMECMP_PARTIAL_DIR`), so its reconstruction
            // reads the entry it is writing and a final `ftruncate` sizes the
            // result. This executor still picks its basis from the destination
            // or a `--fuzzy` candidate only, so the partial entry is pure
            // output: nothing reads it back, and its old tail has to go on open
            // or it would survive past a shorter result. When the basis half is
            // ported this becomes `delta_signature.is_none()`, exactly as
            // `WriteStrategy::Inplace` above.
            let should_truncate = true;
            debug_log!(
                Io,
                3,
                "one_inplace staging for {} into {}",
                record_path.display(),
                partial_file.display()
            );
            let (new_guard, file) = DestinationWriteGuard::new_one_inplace(
                destination,
                partial_file,
                partial_dir,
                should_truncate,
                Some(context.destination_root()),
            )?;
            *staging_path = Some(new_guard.staging_path().to_path_buf());
            *guard = Some(new_guard);
            Ok(file)
        }
        WriteStrategy::Direct => {
            // Direct write when there is no existing file to protect.
            //
            // Upstream has no counterpart to this arm: receiver.c ALWAYS stages
            // into a `.name.XXXXXX` temp (or --temp-dir) and commits with a
            // rename, so it never opens the final name and inherits the
            // parent-anchored confinement for free. `Direct` is an oc
            // optimisation for a not-yet-existing destination, so it has to
            // anchor the parent itself - otherwise it re-resolves `destination`
            // by path and follows a parent flipped to a symlink mid-transfer,
            // which is the same escape the commit rename closes above and the
            // one `keep_dirlinks_refuses_a_destination_symlink_pointing_outside_the_tree`
            // catches on platforms without O_TMPFILE.
            //
            // `O_EXCL` is preserved from the path-based form: it is what makes a
            // concurrent writer lose the race with EEXIST rather than share the
            // file.
            debug_log!(
                Io,
                3,
                "direct write to {} (no existing destination)",
                record_path.display()
            );
            #[cfg(unix)]
            {
                fast_io::confined_create_new(context.destination_root(), destination)
                    .map_err(|error| LocalCopyError::io("copy file", destination, error))
            }
            #[cfg(not(unix))]
            {
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(destination)
                    .map_err(|error| LocalCopyError::io("copy file", destination, error))
            }
        }
        WriteStrategy::AnonymousTempFile => {
            #[cfg(target_os = "linux")]
            {
                match DestinationWriteGuard::new_anonymous(
                    destination,
                    Some(context.destination_root()),
                ) {
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
            let (new_guard, file) = DestinationWriteGuard::new_confined(
                destination,
                partial_enabled,
                context.partial_directory_path(),
                context.temp_directory_path(),
                Some(context.destination_root()),
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
            let (new_guard, file) = DestinationWriteGuard::new_confined(
                destination,
                partial_enabled,
                context.partial_directory_path(),
                context.temp_directory_path(),
                Some(context.destination_root()),
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
            false,
            Path::new(NO_TMPFILE),
        )
    }

    /// `strategy()` with upstream's `one_inplace` gate turned on.
    fn strategy_one_inplace(partial: bool, existing: bool) -> WriteStrategy {
        select_write_strategy(
            0,
            false,
            partial,
            false,
            existing,
            false,
            true,
            Path::new(NO_TMPFILE),
        )
    }

    #[test]
    fn append_outranks_one_inplace_staging() {
        // The append offset is measured against the destination, not against
        // the partial-dir entry one_inplace would write into, so the append
        // strategy keeps precedence. See the note on `select_write_strategy`.
        assert_eq!(
            select_write_strategy(
                1024,
                false,
                true,
                false,
                true,
                false,
                true,
                Path::new(NO_TMPFILE),
            ),
            WriteStrategy::Append
        );
    }

    #[test]
    fn existing_partial_dir_entry_selects_one_inplace_staging() {
        // upstream: receiver.c:1195-1196 - `fnametmp = one_inplace ? partialptr
        // : fname`. Without this the same inputs pick TempFileRename and the
        // partial-dir entry is never opened at all.
        assert_eq!(
            strategy_one_inplace(true, false),
            WriteStrategy::InplacePartialDir
        );
        assert_eq!(
            strategy_one_inplace(true, true),
            WriteStrategy::InplacePartialDir
        );
        assert_eq!(
            strategy(0, false, true, false, false, false),
            WriteStrategy::TempFileRename
        );
    }

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

    #[test]
    fn no_existing_dest_selects_direct_strategy() {
        assert_eq!(
            strategy(0, false, false, false, false, false),
            WriteStrategy::Direct
        );
    }

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

    #[test]
    fn partial_prevents_anonymous_strategy() {
        // Even on a real tmpdir, partial forces TempFileRename because partial
        // files need a visible staging path.
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        let result = select_write_strategy(0, false, true, false, true, false, false, &dest);
        assert_eq!(result, WriteStrategy::TempFileRename);
    }

    #[test]
    fn temp_dir_prevents_anonymous_strategy() {
        // --temp-dir prevents anonymous because linkat cannot cross devices.
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        let result = select_write_strategy(0, false, false, false, true, true, false, &dest);
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
    fn anonymous_preferred_over_direct_for_new_files() {
        // When O_TMPFILE is available, it should be preferred even for new files
        // where Direct would otherwise be selected. O_TMPFILE provides atomic
        // appearance and kernel auto-cleanup on crash.
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("new_file.txt");
        if !can_use_anonymous_tmpfile(&dest) {
            return;
        }
        // No existing dest, no partial, no delay, no temp-dir - would be Direct
        // without O_TMPFILE, but AnonymousTempFile is preferred when available.
        let result = select_write_strategy(0, false, false, false, false, false, &dest);
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

    #[cfg(target_os = "linux")]
    #[test]
    fn anonymous_preferred_over_temp_file_rename() {
        // When O_TMPFILE is available and no partial/temp-dir flags, anonymous
        // should be chosen instead of TempFileRename for existing destinations.
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("existing.txt");
        if !can_use_anonymous_tmpfile(&dest) {
            return;
        }
        let result = select_write_strategy(0, false, false, false, true, false, &dest);
        assert_eq!(result, WriteStrategy::AnonymousTempFile);
    }

    #[test]
    fn direct_used_when_o_tmpfile_unavailable_and_no_existing_dest() {
        // Fallback: when O_TMPFILE is not available, Direct is used for new
        // files with no special flags.
        assert_eq!(
            strategy(0, false, false, false, false, false),
            WriteStrategy::Direct
        );
    }

    #[test]
    fn can_use_anonymous_returns_false_for_nonexistent_dir() {
        assert!(!can_use_anonymous_tmpfile(Path::new(
            "/no_such_dir_tmpfile_test/file"
        )));
    }
}
