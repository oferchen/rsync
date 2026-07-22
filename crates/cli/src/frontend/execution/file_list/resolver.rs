//! Path resolution and validation for file list entries.
//!
//! Resolves file list entries against the source base directory, inserting
//! `.` markers for `--files-from` to preserve relative path structure.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use super::parser::operand_is_remote;

/// Returns `true` when the operand contains a `/./` or leading `./` marker
/// that upstream rsync uses to split the chdir prefix from the transferred
/// relative filename.
///
/// # Upstream Reference
///
/// - `flist.c:2351` - `strstr(fbuf, "/./")` detects embedded marker
fn entry_contains_dot_marker(operand: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = operand.as_bytes();
        // Check for "/./", or a leading "./" (equivalent to no prefix)
        if bytes.starts_with(b"./") {
            return true;
        }
        bytes.windows(3).any(|w| w == b"/./")
    }

    #[cfg(not(unix))]
    {
        let text = operand.to_string_lossy();
        text.starts_with("./") || text.contains("/./")
    }
}

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
/// - `options.c:2205-2206` - `--files-from` implies `--relative`
/// - `main.c:789-799` - source dir used as chdir base for file list entries
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
            // upstream: flist.c:2351-2353 - when relative_paths is set and a
            // file list entry contains "/./", upstream splits the entry at that
            // marker: the portion before becomes a chdir prefix (relative to
            // argv[0]), and the portion after becomes the transferred filename.
            //
            // If the entry already carries its own "./" marker (e.g.
            // "from/./dir/file"), join base + entry directly so the engine's
            // detect_marker_components finds the entry's own marker. Adding a
            // second "./" would create base/./from/./dir/file which makes the
            // engine split at the first marker, incorrectly keeping "from/" in
            // the destination path.
            if entry_contains_dot_marker(entry.as_os_str()) {
                let mut combined = base_path.to_path_buf();
                combined.push(entry_path);
                *entry = combined.into_os_string();
            } else {
                let mut combined = base_path.to_path_buf();
                combined.push(".");
                combined.push(entry_path);
                *entry = combined.into_os_string();
            }
        } else {
            let mut combined = base_path.to_path_buf();
            combined.push(entry_path);
            *entry = combined.into_os_string();
        }
    }
}
