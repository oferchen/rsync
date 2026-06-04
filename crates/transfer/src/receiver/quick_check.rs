//! Quick-check logic for determining whether files need transfer.
//!
//! Implements upstream rsync's `quick_check_ok()` algorithm comparing
//! destination stat against source file list entries, plus reference
//! directory handling and file checksum comparison.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use logging::info_log;
use protocol::flist::FileEntry;

use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};
use crate::delta_apply::ChecksumVerifier;

use metadata::{MetadataOptions, apply_metadata_with_cached_stat};
use protocol::acl::AclCache;

use super::apply_acls_from_receiver_cache;

/// Returns true if this file entry is a hardlink follower that should be
/// created as a hard link rather than transferred via delta.
///
/// A follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST. Leaders have
/// both flags set. Entries without hardlink flags return false.
///
/// # Upstream Reference
///
/// - `generator.c:1539` - `F_HLINK_NOT_FIRST(file)` check
/// - `hlink.c:284` - `hard_link_check()` called for non-first entries
pub(super) fn is_hardlink_follower(entry: &FileEntry) -> bool {
    entry.flags().hlinked() && !entry.flags().hlink_first()
}

/// Pure-function quick-check: compares destination stat against source entry.
///
/// Returns `true` when the destination file matches the source entry (skip transfer).
///
/// Follows upstream `generator.c:617 quick_check_ok()` evaluation order:
/// 1. Size mismatch - always needs transfer
/// 2. `always_checksum` - compute file checksum and compare (ignores mtime)
/// 3. `size_only` - size matched, skip transfer
/// 4. `!preserve_times` (implies `ignore_times`) - force transfer
/// 5. mtime comparison
pub(super) fn quick_check_matches(
    entry: &FileEntry,
    dest_path: &Path,
    dest_meta: &fs::Metadata,
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
) -> bool {
    // upstream: generator.c:621 - size check first
    if dest_meta.len() != entry.size() {
        return false;
    }
    // upstream: generator.c:626 - always_checksum compares file checksums
    // instead of relying on mtime. Takes priority over size_only and ignore_times.
    if let Some(algorithm) = always_checksum {
        return match entry.checksum() {
            Some(expected) => {
                file_checksum_matches(dest_path, dest_meta.len(), algorithm, expected)
            }
            None => false,
        };
    }
    // upstream: generator.c:632 - `if (size_only) return 1;` after size match
    if size_only {
        return true;
    }
    // upstream: generator.c:635 - ignore_times forces transfer
    if !preserve_times {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() == entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| d.as_secs() as i64 == entry.mtime())
    }
}

/// Returns `true` when the destination file's mtime is strictly newer than the source.
///
/// Used by `--update` (`-u`) to skip files where the destination is already newer.
///
/// upstream: generator.c:1709 - `file->modtime - sx.st.st_mtime < modify_window`
/// with modify_window=0, this simplifies to `dest_mtime > source_mtime`.
pub(super) fn dest_mtime_newer(dest_meta: &fs::Metadata, source_entry: &FileEntry) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() > source_entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| (d.as_secs() as i64) > source_entry.mtime())
    }
}

/// Checks reference directories for a file that matches the source entry.
///
/// When the destination file does not exist, this function iterates through
/// configured reference directories (`--compare-dest`, `--copy-dest`,
/// `--link-dest`) and performs the appropriate action based on kind:
///
/// - `Compare`: skip transfer entirely (file is up-to-date in reference)
/// - `Link`: create a hard link from the reference file to the destination
/// - `Copy`: copy the reference file to the destination
///
/// Returns `true` if the entry was handled and should not be transferred.
///
/// # Upstream Reference
///
/// - `generator.c:942` - `try_dests_reg()` iterates `basis_dir[]`
/// - `generator.c:983` - match_level 3 with `COMPARE_DEST` returns -2 (skip)
/// - `generator.c:991` - match_level 3 with `LINK_DEST` calls `hard_link_one()`
/// - `generator.c:1021` - match_level >= 2 with `COPY_DEST` copies locally
#[allow(clippy::too_many_arguments)]
pub(super) fn try_reference_dest(
    entry: &FileEntry,
    dest_dir: &Path,
    reference_directories: &[ReferenceDirectory],
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
    metadata_opts: &MetadataOptions,
    metadata_errors: &mut Vec<(PathBuf, String)>,
    acl_cache: Option<&AclCache>,
) -> bool {
    if reference_directories.is_empty() {
        return false;
    }

    let relative_path = entry.path();
    for ref_dir in reference_directories {
        let ref_path = ref_dir.path.join(relative_path);
        let ref_meta = match fs::metadata(&ref_path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };

        // upstream: generator.c:959 - quick_check_ok against reference file
        if !quick_check_matches(
            entry,
            &ref_path,
            &ref_meta,
            preserve_times,
            size_only,
            always_checksum,
        ) {
            continue;
        }

        let dest_path = dest_dir.join(relative_path);
        match ref_dir.kind {
            ReferenceDirectoryKind::Compare => {
                // upstream: generator.c:1007 - return -2 (file is up-to-date)
                return true;
            }
            ReferenceDirectoryKind::Link => {
                // upstream: generator.c:991 - hard_link_one()
                if let Some(parent) = dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                // Try io_uring LINKAT on Linux 5.15+, fall back to std::fs::hard_link.
                if fast_io::hard_link(&ref_path, &dest_path).is_ok() {
                    // Skip the stat syscall inside apply_metadata_from_file_entry:
                    // the hard link shares the reference file's inode, and we
                    // unconditionally apply the desired ownership/permissions.
                    if let Err(e) =
                        apply_metadata_with_cached_stat(&dest_path, entry, metadata_opts, None)
                    {
                        metadata_errors.push((dest_path.clone(), e.to_string()));
                    }
                    if let Err(e) = apply_acls_from_receiver_cache(
                        &dest_path,
                        entry,
                        acl_cache,
                        !entry.is_symlink(),
                    ) {
                        metadata_errors.push((dest_path, e.to_string()));
                    }
                    return true;
                }
            }
            ReferenceDirectoryKind::Copy => {
                // upstream: generator.c:1021 - copy_altdest_file()
                if let Some(parent) = dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                match fs::copy(&ref_path, &dest_path) {
                    Ok(_) => {
                        // Skip the stat inside apply_metadata_from_file_entry:
                        // we just created this file, so its metadata does not
                        // match the desired entry yet. Pass None to apply
                        // unconditionally without a redundant stat.
                        if let Err(e) =
                            apply_metadata_with_cached_stat(&dest_path, entry, metadata_opts, None)
                        {
                            metadata_errors.push((dest_path.clone(), e.to_string()));
                        }
                        if let Err(e) = apply_acls_from_receiver_cache(
                            &dest_path,
                            entry,
                            acl_cache,
                            !entry.is_symlink(),
                        ) {
                            metadata_errors.push((dest_path, e.to_string()));
                        }
                        return true;
                    }
                    Err(error) => {
                        // upstream: generator.c:919 - rsyserr(FINFO, errno,
                        // "copy_file %s => %s", full_fname(src), copy_to)
                        // under INFO_GTE(COPY, 1). The flag is part of
                        // info_verbosity[1] (options.c:241) so this fires at
                        // `-v` or `--info=COPY`. Wording mirrors upstream's
                        // rsyserr format: `copy_file SRC => DST: ERRSTR (ERRNO)`.
                        let errno = error.raw_os_error().unwrap_or(0);
                        info_log!(
                            Copy,
                            1,
                            "copy_file {} => {}: {} ({})",
                            ref_path.display(),
                            dest_path.display(),
                            io_error_message(&error),
                            errno
                        );
                    }
                }
            }
        }
    }
    false
}

/// Strips the trailing `" (os error N)"` suffix that `std::io::Error::Display`
/// appends to OS errors so the rendered message matches upstream `rsyserr`'s
/// `strerror(errno)` form. Non-OS errors are returned as-is.
fn io_error_message(error: &std::io::Error) -> String {
    let display = error.to_string();
    if let Some(errno) = error.raw_os_error() {
        let suffix = format!(" (os error {errno})");
        if let Some(trimmed) = display.strip_suffix(&suffix) {
            return trimmed.to_string();
        }
    }
    display
}

/// Computes a file's checksum and compares it against an expected value.
///
/// Used by `--checksum` (`-c`) mode to compare file contents instead of
/// mtime+size quick-check. Returns `true` when checksums match (skip transfer).
///
/// upstream: checksum.c:402 `file_checksum()` - plain hash, no seed
fn file_checksum_matches(
    path: &Path,
    file_size: u64,
    algorithm: protocol::ChecksumAlgorithm,
    expected: &[u8],
) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
    let mut buf = [0u8; 64 * 1024];
    let mut remaining = file_size;
    while remaining > 0 {
        let to_read = buf.len().min(remaining as usize);
        if file.read_exact(&mut buf[..to_read]).is_err() {
            return false;
        }
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }
    let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = hasher.finalize_into(&mut digest);
    // upstream: flist_csum_len determines comparison length
    let cmp_len = expected.len().min(len);
    digest[..cmp_len] == expected[..cmp_len]
}

/// Returns `true` if any component of the path is `..`.
///
/// This mirrors upstream rsync's `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
/// check that rejects paths containing parent-directory references, preventing
/// directory traversal attacks from a malicious sender.
///
/// # Upstream Reference
///
/// - `util1.c`: `clean_fname()` with `CFN_REFUSE_DOT_DOT_DIRS`
pub(super) fn path_contains_dot_dot(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod io_uring_linkat_tests {
    use std::fs;

    /// Verifies `fast_io::hard_link` creates a valid hard link regardless of
    /// whether io_uring handles it or `std::fs::hard_link` does.
    #[test]
    fn hard_link_via_io_uring_or_fallback_in_quick_check() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("linkdest_src.txt");
        let dst = dir.path().join("linkdest_dst.txt");

        fs::write(&src, b"link-dest payload").unwrap();

        fast_io::hard_link(&src, &dst).unwrap();

        assert!(src.exists());
        assert!(dst.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"link-dest payload");
    }

    /// Verifies the fallback correctly fails when the source does not exist.
    #[test]
    fn hard_link_fails_for_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("missing.txt");
        let dst = dir.path().join("dst.txt");

        let result = fast_io::hard_link(&src, &dst);
        assert!(result.is_err());
    }
}

/// Pinning tests for `--info=COPY` emissions on `--copy-dest` failures.
///
/// upstream: generator.c:919 - `INFO_GTE(COPY, 1)` gates an `rsyserr` call in
/// `copy_altdest_file()` whose wording reads:
///   `copy_file SRC => DST: STRERROR (ERRNO)`
/// COPY sits in `info_verbosity[1]` (options.c:241) so the flag is enabled at
/// `-v` and any explicit `--info=COPY`.
#[cfg(unix)]
#[cfg(test)]
mod info_copy_emission_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};
    use metadata::MetadataOptions;
    use protocol::flist::FileEntry;

    use super::try_reference_dest;
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    fn copy_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Copy,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn init_copy_level(level: u8) {
        let mut cfg = VerbosityConfig::from_verbose_level(0);
        cfg.info.copy = level;
        init(cfg);
        let _ = drain_events();
    }

    /// Verifies that when `--copy-dest` cannot write the destination,
    /// `try_reference_dest` emits the upstream-format COPY notice via the
    /// diagnostic queue.
    #[test]
    fn copy_dest_failure_emits_info_copy_notice() {
        init_copy_level(1);

        let temp = tempfile::tempdir().expect("tempdir");
        let ref_dir = temp.path().join("ref");
        fs::create_dir_all(&ref_dir).expect("create ref dir");

        // Populate the alternate-base file the receiver wants to copy.
        let relative = PathBuf::from("payload.bin");
        let payload = b"alt-base contents";
        fs::write(ref_dir.join(&relative), payload).expect("write ref file");

        // Make the destination directory read-only so `fs::copy` fails.
        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");
        let mut perms = fs::metadata(&dest_dir).expect("dest meta").permissions();
        perms.set_mode(0o500);
        fs::set_permissions(&dest_dir, perms).expect("chmod dest");

        let entry = FileEntry::new_file(relative.clone(), payload.len() as u64, 0o644);
        let reference = ReferenceDirectory {
            kind: ReferenceDirectoryKind::Copy,
            path: ref_dir.clone(),
        };
        let metadata_opts = MetadataOptions::default();
        let mut metadata_errors = Vec::new();

        let handled = try_reference_dest(
            &entry,
            &dest_dir,
            std::slice::from_ref(&reference),
            false,
            true,
            None,
            &metadata_opts,
            &mut metadata_errors,
            None,
        );

        // Restore writable permissions so tempdir cleanup succeeds.
        let mut restore = fs::metadata(&dest_dir).expect("dest meta").permissions();
        restore.set_mode(0o700);
        let _ = fs::set_permissions(&dest_dir, restore);

        assert!(
            !handled,
            "expected failed alt-base copy to bubble up as unhandled"
        );

        let messages = copy_messages();
        assert!(
            messages.iter().any(|m| m.starts_with("copy_file ")
                && m.contains(" => ")
                && m.contains("payload.bin")),
            "expected upstream-format COPY,1 notice; got {messages:?}"
        );
    }

    /// Verifies that `--info=nocopy` (level 0) suppresses the emission,
    /// mirroring upstream's `INFO_GTE(COPY, 1)` gate.
    #[test]
    fn nocopy_suppresses_info_copy_notice() {
        init_copy_level(0);

        let temp = tempfile::tempdir().expect("tempdir");
        let ref_dir = temp.path().join("ref");
        fs::create_dir_all(&ref_dir).expect("create ref dir");
        let relative = PathBuf::from("muted.bin");
        let payload = b"silent payload";
        fs::write(ref_dir.join(&relative), payload).expect("write ref file");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");
        let mut perms = fs::metadata(&dest_dir).expect("dest meta").permissions();
        perms.set_mode(0o500);
        fs::set_permissions(&dest_dir, perms).expect("chmod dest");

        let entry = FileEntry::new_file(relative.clone(), payload.len() as u64, 0o644);
        let reference = ReferenceDirectory {
            kind: ReferenceDirectoryKind::Copy,
            path: ref_dir,
        };
        let metadata_opts = MetadataOptions::default();
        let mut metadata_errors = Vec::new();

        let _ = try_reference_dest(
            &entry,
            &dest_dir,
            std::slice::from_ref(&reference),
            false,
            true,
            None,
            &metadata_opts,
            &mut metadata_errors,
            None,
        );

        let mut restore = fs::metadata(&dest_dir).expect("dest meta").permissions();
        restore.set_mode(0o700);
        let _ = fs::set_permissions(&dest_dir, restore);

        // Restore the default so later tests in this thread see the upstream baseline.
        init(VerbosityConfig::from_verbose_level(0));
        let _ = drain_events();

        assert!(
            copy_messages().is_empty(),
            "COPY notice must be gated at level 1"
        );
    }
}
