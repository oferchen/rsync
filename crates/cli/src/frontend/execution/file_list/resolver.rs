//! Path resolution and validation for file list entries.
//!
//! Resolves file list entries against the source base directory, inserting
//! `.` markers for `--files-from` to preserve relative path structure.

use std::ffi::OsString;
use std::path::Path;

use super::parser::operand_is_remote;

/// Resolves file list entries against the source base directory.
///
/// When `files_from_active` is true, entries are joined with a `./` marker
/// between the base directory and the relative entry path. This enables
/// the `--relative` flag (implied by `--files-from`) to preserve only the
/// listed path structure at the destination, not the full base directory
/// hierarchy.
///
/// For example, with base `/src` and entry `file.txt`, the result is
/// `/src/./file.txt`. The engine's `relative_root()` strips everything
/// up to and including the `.` marker, yielding `file.txt` at the
/// destination.
///
/// When `files_from_active` is false, the legacy behaviour applies: entries
/// are simply joined to the base path without a marker (only when
/// `--relative` is not explicitly enabled).
///
/// # Upstream Reference
///
/// - `options.c:2187-2188` - `--files-from` implies `--relative`
/// - `main.c:780-790` - source dir used as chdir base for file list entries
pub(crate) fn resolve_file_list_entries(
    entries: &mut [OsString],
    explicit_operands: &[OsString],
    relative_enabled: bool,
    files_from_active: bool,
) {
    if entries.is_empty() || explicit_operands.len() <= 1 {
        return;
    }

    let base_sources = &explicit_operands[..explicit_operands.len() - 1];
    if base_sources.len() != 1 {
        return;
    }

    let base = &base_sources[0];
    if operand_is_remote(base.as_os_str()) {
        return;
    }

    // When --files-from is active, always resolve entries with a ./
    // marker so --relative preserves only the listed structure.
    // Without --files-from, skip resolution when --relative is on
    // (legacy behaviour).
    if !files_from_active && relative_enabled {
        return;
    }

    let base_path = Path::new(base);
    for entry in entries.iter_mut() {
        if entry.is_empty() {
            continue;
        }

        if operand_is_remote(entry.as_os_str()) {
            continue;
        }

        let entry_path = Path::new(entry);
        if entry_path.is_absolute() {
            continue;
        }

        if files_from_active {
            // Insert ./ marker: base/./entry - upstream rsync uses the
            // source dir as a chdir target and entries are relative to it.
            // The marker tells the engine where the relative portion begins.
            let mut combined = base_path.to_path_buf();
            combined.push(".");
            combined.push(entry_path);
            *entry = combined.into_os_string();
        } else {
            let mut combined = base_path.to_path_buf();
            combined.push(entry_path);
            *entry = combined.into_os_string();
        }
    }
}
