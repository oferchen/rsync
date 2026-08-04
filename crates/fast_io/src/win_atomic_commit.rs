//! No-follow atomic file commit primitives for the Windows receiver.
//!
//! These mirror, on Windows, the Unix receiver's reparse-point TOCTOU
//! hardening (the residual half of CVE-2024-12747). On Unix the receiver
//! creates its temp file with `openat(dirfd, leaf, O_CREAT | O_EXCL |
//! O_NOFOLLOW)` and commits it with `renameat(dirfd, leaf, dirfd, leaf)`, both
//! resolved against a directory file descriptor pinned at receiver setup (see
//! [`crate::dir_sandbox`]). That anchoring means an attacker who swaps a path
//! component for a symlink/junction/mount-point between the check and the use
//! cannot redirect the write or the final rename outside the destination tree.
//!
//! Windows has no `openat`/`renameat`, but the same guarantee is available via:
//!
//! - `create_new_no_follow` - `CreateFileW`-equivalent open with
//!   `CREATE_NEW` + `FILE_FLAG_OPEN_REPARSE_POINT`, so a reparse point planted
//!   at the temp leaf is opened as the reparse point itself (and `CREATE_NEW`
//!   then fails) rather than traversed. This is the analog of `O_EXCL |
//!   O_NOFOLLOW`.
//! - `rename_no_follow` - a handle-based commit rename. The destination
//!   parent is opened no-follow, validated as a real directory (not a reparse
//!   point), and *pinned* (shared without `FILE_SHARE_DELETE`) so it cannot be
//!   renamed/removed/replaced with a junction while the rename runs; the temp
//!   handle is then renamed via `SetFileInformationByHandle(FileRenameInfo)`.
//!   The Win32 `SetFileInformationByHandle` rejects a non-NULL
//!   `FILE_RENAME_INFO::RootDirectory` (`ERROR_INVALID_PARAMETER`), so the
//!   anchoring is provided by the pinned, validated parent handle rather than a
//!   handle-relative name. This closes the same reparse-point redirect that the
//!   Unix `renameat`-on-a-dirfd closes for the final directory component.
//!
//! All Win32 FFI lives here in `fast_io` (a permitted-unsafe crate); the
//! `transfer` receiver calls the safe functions.

/// Extended-length (`\\?\`) path conversion shared by the no-follow commit
/// primitives.
///
/// The handle-based opens in [`imp`] all go through `OpenOptions::open`, which
/// applies std's `maybe_verbatim` internally, so they already accept paths over
/// the legacy 260-char `MAX_PATH`. The one Win32 call that receives a raw path
/// string is `SetFileInformationByHandle(FileRenameInfo)` in
/// [`imp::set_rename_info`]: its `FILE_RENAME_INFO::FileName` is passed straight
/// to the kernel with no long-path awareness, so a deep destination fails with
/// `ERROR_FILENAME_EXCED_RANGE` (206). [`to_verbatim_wide`] converts an
/// already-absolute, already-normalized destination path into the `\\?\`
/// verbatim form the way std does, restoring the long-path behaviour the prior
/// `std::fs::rename` commit path had.
#[cfg_attr(not(windows), allow(dead_code))]
mod verbatim {
    // UTF-16 code units used to detect and build verbatim paths. All are ASCII,
    // so casting the byte literal to `u16` is exact.
    const SEP: u16 = b'\\' as u16;
    const ALT_SEP: u16 = b'/' as u16;
    const QUERY: u16 = b'?' as u16;
    const COLON: u16 = b':' as u16;

    // `\\?\`
    const VERBATIM_PREFIX: [u16; 4] = [SEP, SEP, QUERY, SEP];
    // `\??\` (the NT-namespace form std also leaves untouched)
    const NT_PREFIX: [u16; 4] = [SEP, QUERY, QUERY, SEP];
    // `\\?\UNC\`
    const UNC_VERBATIM_PREFIX: [u16; 8] = [
        SEP,
        SEP,
        QUERY,
        SEP,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEP,
    ];

    /// True for an ASCII drive letter (`A`-`Z` / `a`-`z`) as a UTF-16 unit.
    fn is_drive_letter(c: u16) -> bool {
        (b'A' as u16..=b'Z' as u16).contains(&c) || (b'a' as u16..=b'z' as u16).contains(&c)
    }

    /// Returns the extended-length (`\\?\`) verbatim form of a UTF-16 Windows
    /// path, or the input unchanged when it is already verbatim/NT-prefixed or
    /// is not fully qualified.
    ///
    /// Operates on a UTF-16 code-unit slice (as std's `maybe_verbatim` does) so
    /// it is unit-testable on every platform even though it is only used on
    /// Windows. The commit path only ever hands this a fully-qualified,
    /// already-normalized destination, so - unlike std - it does not resolve
    /// `.`/`..` via `GetFullPathNameW`; it only rewrites the prefix:
    ///
    /// - Already `\\?\` or `\??\`: returned untouched.
    /// - Drive-absolute (`X:\...` or `X:/...`): becomes `\\?\X:\...` with
    ///   forward slashes normalized to backslashes.
    /// - UNC (`\\server\...` or `//server/...`): becomes `\\?\UNC\server\...`.
    /// - Anything else (relative, drive-relative like `X:foo`): returned
    ///   untouched - a verbatim path must be fully qualified, and such paths
    ///   never exceed `MAX_PATH` on the commit path.
    pub(super) fn to_verbatim_wide(path: &[u16]) -> Vec<u16> {
        if path.starts_with(&VERBATIM_PREFIX) || path.starts_with(&NT_PREFIX) {
            return path.to_vec();
        }

        // Drive-absolute: `X:\...` / `X:/...`.
        if path.len() >= 3
            && is_drive_letter(path[0])
            && path[1] == COLON
            && (path[2] == SEP || path[2] == ALT_SEP)
        {
            let mut out = Vec::with_capacity(VERBATIM_PREFIX.len() + path.len());
            out.extend_from_slice(&VERBATIM_PREFIX);
            out.extend(path.iter().map(|&c| if c == ALT_SEP { SEP } else { c }));
            return out;
        }

        // UNC: `\\server\...` / `//server/...`. Drop the leading two separators;
        // the `\\?\UNC\` prefix supplies them.
        if path.len() >= 2
            && (path[0] == SEP || path[0] == ALT_SEP)
            && (path[1] == SEP || path[1] == ALT_SEP)
        {
            let mut out = Vec::with_capacity(UNC_VERBATIM_PREFIX.len() + path.len());
            out.extend_from_slice(&UNC_VERBATIM_PREFIX);
            out.extend(
                path[2..]
                    .iter()
                    .map(|&c| if c == ALT_SEP { SEP } else { c }),
            );
            return out;
        }

        path.to_vec()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Encodes an ASCII string as a UTF-16 vector, the wire form Win32 sees.
        fn w(s: &str) -> Vec<u16> {
            s.encode_utf16().collect()
        }

        /// A deep drive-absolute path (>260 UTF-16 units) gains the `\\?\`
        /// prefix - the exact fix for the `ERROR_FILENAME_EXCED_RANGE` commit.
        #[test]
        fn long_absolute_path_gains_verbatim_prefix() {
            let deep = format!("C:\\{}\\file.bin", "d0000000".repeat(40));
            assert!(deep.len() > 260, "fixture must exceed MAX_PATH");
            let got = to_verbatim_wide(&w(&deep));
            assert_eq!(got, w(&format!("\\\\?\\{deep}")));
        }

        /// A short drive-absolute path is still prefixed (correctness does not
        /// depend on length; the raw rename would otherwise stay short-only).
        #[test]
        fn short_absolute_path_gains_verbatim_prefix() {
            assert_eq!(
                to_verbatim_wide(&w(r"C:\dir\file.bin")),
                w(r"\\?\C:\dir\file.bin")
            );
        }

        /// Forward slashes in a drive-absolute path are normalized to
        /// backslashes: the verbatim namespace does no separator translation.
        #[test]
        fn forward_slashes_normalized_to_backslashes() {
            assert_eq!(
                to_verbatim_wide(&w("C:/dir/sub/file.bin")),
                w(r"\\?\C:\dir\sub\file.bin")
            );
        }

        /// A UNC path maps to the `\\?\UNC\` form with the leading `\\` dropped.
        #[test]
        fn unc_path_maps_to_unc_verbatim() {
            assert_eq!(
                to_verbatim_wide(&w(r"\\server\share\dir\file.bin")),
                w(r"\\?\UNC\server\share\dir\file.bin")
            );
        }

        /// An already-verbatim path is returned untouched (idempotent), so a
        /// double conversion cannot corrupt the prefix.
        #[test]
        fn already_verbatim_is_unchanged() {
            let p = w(r"\\?\C:\dir\file.bin");
            assert_eq!(to_verbatim_wide(&p), p);
        }

        /// An NT-namespace (`\??\`) path is likewise left untouched.
        #[test]
        fn nt_prefixed_is_unchanged() {
            let p = w(r"\??\C:\dir\file.bin");
            assert_eq!(to_verbatim_wide(&p), p);
        }

        /// A relative path is not fully qualified, so it is returned unchanged
        /// (no prefix, no slash rewrite) - such paths never exceed MAX_PATH on
        /// the commit path.
        #[test]
        fn relative_path_is_unchanged() {
            let p = w(r"dir\file.bin");
            assert_eq!(to_verbatim_wide(&p), p);
        }

        /// A drive-relative path (`X:foo`, no separator after the colon) is also
        /// left unchanged - prefixing it would change its meaning.
        #[test]
        fn drive_relative_path_is_unchanged() {
            let p = w("C:file.bin");
            assert_eq!(to_verbatim_wide(&p), p);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::thread::sleep;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileRenameInfo, GetFileInformationByHandle, SetFileInformationByHandle,
    };

    /// Shared-access mask allowing concurrent readers, writers, and deleters so
    /// the handle-based rename works while other handles (e.g. antivirus) are
    /// open. Mirrors upstream's tolerance for external readers.
    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    /// Atomically creates `path` with `CREATE_NEW` semantics without following a
    /// reparse point at the final component - the Windows analog of `O_CREAT |
    /// O_EXCL | O_NOFOLLOW`.
    ///
    /// `FILE_FLAG_OPEN_REPARSE_POINT` makes a symlink/junction pre-planted at
    /// `path` open as the reparse point itself rather than being traversed, so
    /// `CREATE_NEW` fails with `ERROR_FILE_EXISTS` (surfaced as
    /// [`io::ErrorKind::AlreadyExists`], which the receiver's temp-name loop
    /// retries) instead of creating the file through the attacker-controlled
    /// link. The handle is granted `DELETE` access and shared for delete so the
    /// later [`rename_no_follow`] commit can rename it by handle.
    ///
    /// # Errors
    ///
    /// Propagates the underlying open failure. A pre-existing name yields
    /// [`io::ErrorKind::AlreadyExists`]; a missing parent yields
    /// [`io::ErrorKind::NotFound`].
    pub fn create_new_no_follow(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    /// Commits `temp_path` to `dest_path` with a handle-anchored rename that a
    /// concurrent reparse-point swap on the destination parent cannot redirect.
    ///
    /// Steps (the Windows analog of pinning the destination dirfd on Unix):
    ///
    /// 1. Open, validate, and *pin* the destination parent directory. The
    ///    no-follow open (`FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_
    ///    POINT`) yields the directory itself rather than traversing a
    ///    junction/mount-point planted at that path, and is rejected unless it
    ///    is a real directory (not a reparse point). Sharing omits
    ///    `FILE_SHARE_DELETE`, so the directory cannot be renamed, removed, or
    ///    replaced while the handle is held - it cannot be swapped for a reparse
    ///    point in the check-to-use window before the rename.
    /// 2. Open `temp_path` with `FILE_FLAG_OPEN_REPARSE_POINT` + `DELETE` access
    ///    (required by `FileRenameInfo`) and reject it if it resolved to a
    ///    reparse point - closing a swap of the temp leaf on the source side.
    /// 3. Rename the temp handle via `SetFileInformationByHandle(FileRenameInfo)`.
    ///    `RootDirectory` is `NULL` - the Win32 `SetFileInformationByHandle`
    ///    rejects a non-NULL `RootDirectory` with `ERROR_INVALID_PARAMETER` - so
    ///    `FileName` carries the full destination path. The redirect protection
    ///    comes from the pinned, validated parent handle held open across the
    ///    call (step 1), not from a handle-relative name.
    ///
    /// `replace_existing` maps to `FILE_RENAME_INFO::ReplaceIfExists` (upstream
    /// `do_rename` overwrites the destination).
    ///
    /// The pin in step 1 omits `FILE_SHARE_DELETE`, which also means two commits
    /// racing to the *same* destination momentarily lock each other out: while
    /// one holds its pin, the other's replace can fail with
    /// `ERROR_SHARING_VIOLATION` (32) or `ERROR_ACCESS_DENIED` (5). That
    /// contention is transient - the pin drops the instant the other commit
    /// returns - so [`is_transient_rename_contention`] gates a bounded retry
    /// loop (`MAX_RETRY_ATTEMPTS` attempts, ~1 s cap) that re-opens and
    /// re-validates the parent on every attempt, preserving the anti-swap
    /// invariant across retries. oc-specific robustness: upstream rsync never
    /// commits two temp files to one destination concurrently, so this only
    /// races in the engine's parallel `DestinationWriteGuard` path, never on the
    /// wire.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] if `dest_path` lacks a parent
    ///   directory.
    /// - An error whose `raw_os_error()` is `ERROR_NOT_SAME_DEVICE` (17) when
    ///   the temp file is on another volume; callers fall back to copy+remove
    ///   (upstream `util1.c:robust_rename()`).
    /// - Any underlying open, validation, or rename failure. A reparse-point
    ///   swap detected in step 1 or 2 surfaces as an error, so the commit fails
    ///   safe rather than following the redirect. Transient same-destination
    ///   contention that outlasts the retry budget surfaces as the last error.
    pub fn rename_no_follow(
        temp_path: &Path,
        dest_path: &Path,
        replace_existing: bool,
    ) -> io::Result<()> {
        let dest_dir = dest_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination path has no parent directory",
            )
        })?;

        // Bounded retry on transient same-destination contention. Each attempt
        // re-opens and re-validates the parent, so the reparse-swap rejection
        // still fires on every attempt (never hoisted out of the loop); the
        // fixed 20 ms backoff caps total waiting at ~1 s.
        let mut attempt: u32 = 0;
        loop {
            match try_rename_no_follow_once(temp_path, dest_dir, dest_path, replace_existing) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    attempt += 1;
                    let transient = err
                        .raw_os_error()
                        .is_some_and(super::is_transient_rename_contention);
                    if transient && attempt < MAX_RETRY_ATTEMPTS {
                        sleep(RETRY_BACKOFF);
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Retry ceiling for [`rename_no_follow`]'s transient-contention loop.
    /// `MAX_RETRY_ATTEMPTS * RETRY_BACKOFF` bounds the total wait at ~1 s.
    const MAX_RETRY_ATTEMPTS: u32 = 50;
    /// Fixed backoff between contention retries.
    const RETRY_BACKOFF: Duration = Duration::from_millis(20);

    /// One attempt of the anchored, reparse-hardened commit rename. Splitting
    /// this out lets [`rename_no_follow`] retry the whole open+validate+rename
    /// sequence on transient same-destination contention while re-running the
    /// reparse-point checks on every attempt.
    fn try_rename_no_follow_once(
        temp_path: &Path,
        dest_dir: &Path,
        dest_path: &Path,
        replace_existing: bool,
    ) -> io::Result<()> {
        // (1) Open, validate, and pin the destination parent. The missing
        // FILE_SHARE_DELETE keeps the directory from being renamed/removed/
        // replaced (swapped for a junction) while this handle is held across
        // the rename below; FILE_FLAG_OPEN_REPARSE_POINT means a junction
        // already planted at the path opens as the reparse point itself and is
        // rejected here rather than traversed.
        let dir = OpenOptions::new()
            .access_mode(FILE_GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dest_dir)?;
        let dir_attrs = file_attributes(&dir)?;
        if dir_attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to commit: destination parent is a reparse point",
            ));
        }
        if dir_attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination parent is not a directory",
            ));
        }

        // (2) Source handle: no-follow open with DELETE access (required by
        // FileRenameInfo). Reject a reparse point swapped in at the temp leaf.
        let src = OpenOptions::new()
            .access_mode(DELETE | FILE_GENERIC_READ)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(temp_path)?;
        if file_attributes(&src)? & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to commit: temp file resolved to a reparse point",
            ));
        }

        // (3) Handle-based rename. RootDirectory=NULL + full destination path;
        // the pinned parent handle (`dir`) is held open across the call so the
        // final directory component cannot be swapped for a reparse point.
        let result = set_rename_info(&src, dest_path.as_os_str(), replace_existing);
        drop(dir);
        result
    }

    /// Returns the file attribute bitmask of an open handle.
    fn file_attributes(file: &File) -> io::Result<u32> {
        // BY_HANDLE_FILE_INFORMATION is a plain-old-data struct (integers and
        // FILETIME fields); a zeroed value is valid before the call fills it.
        // SAFETY: zeroing a POD struct with no invalid bit patterns is sound,
        // and `file` owns a valid, open handle for the duration of the call.
        // `info` is a correctly sized, writable `BY_HANDLE_FILE_INFORMATION`;
        // GetFileInformationByHandle only writes into it and returns 0 on
        // failure.
        #[allow(unsafe_code)]
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let ok = GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(info.dwFileAttributes)
        }
    }

    /// Issues `SetFileInformationByHandle(FileRenameInfo)` on `src` to rename it
    /// to the full path `dest_name`.
    ///
    /// `RootDirectory` is `NULL` (the only form the Win32
    /// `SetFileInformationByHandle` accepts) and `FileName` is the complete
    /// destination path, converted to its `\\?\` verbatim form
    /// ([`super::verbatim::to_verbatim_wide`]) because this raw Win32 call is
    /// not long-path aware - without the prefix a destination deeper than 260
    /// characters fails with `ERROR_FILENAME_EXCED_RANGE`. `FILE_RENAME_INFO` is
    /// a variable-length struct whose trailing `FileName[1]` field is a flexible
    /// array; the buffer is allocated as a `Vec<u64>` so it is large enough for
    /// the path and aligned to the struct's 8-byte (HANDLE) alignment.
    /// `FileNameLength` is the path length in bytes (not UTF-16 code units).
    fn set_rename_info(src: &File, dest_name: &OsStr, replace_existing: bool) -> io::Result<()> {
        let name =
            super::verbatim::to_verbatim_wide(&dest_name.encode_wide().collect::<Vec<u16>>());
        let name_bytes = name.len() * size_of::<u16>();
        // size_of::<FILE_RENAME_INFO>() already includes the 2-byte FileName[1]
        // stub, so header + name_bytes slightly over-allocates - harmless.
        let total = size_of::<FILE_RENAME_INFO>() + name_bytes;
        let words = total.div_ceil(size_of::<u64>());
        let mut buf = vec![0u64; words];
        let base = buf.as_mut_ptr().cast::<u8>();

        // SAFETY: `base` points at a zeroed, 8-byte-aligned buffer of at least
        // `total` bytes (Vec<u64> guarantees the alignment FILE_RENAME_INFO's
        // HANDLE field needs). Every field written below lies within `total`,
        // and exactly `name.len()` u16s are copied into the trailing FileName
        // array, whose offset plus length stays within the allocation. `src`
        // holds a valid handle with DELETE access for the SetFileInformation
        // call, which only reads `total` bytes from `base`.
        #[allow(unsafe_code)]
        unsafe {
            let info = base.cast::<FILE_RENAME_INFO>();
            (*info).Anonymous.ReplaceIfExists = replace_existing;
            (*info).RootDirectory = std::ptr::null_mut();
            (*info).FileNameLength = name_bytes as u32;
            let name_dst = std::ptr::addr_of_mut!((*info).FileName).cast::<u16>();
            std::ptr::copy_nonoverlapping(name.as_ptr(), name_dst, name.len());

            let ok = SetFileInformationByHandle(
                src.as_raw_handle() as HANDLE,
                FileRenameInfo,
                base.cast(),
                total as u32,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use tempfile::tempdir;

        /// Happy path: an anchored commit into an ordinary directory renames the
        /// temp file to the destination leaf and removes the temp file.
        #[test]
        fn rename_no_follow_commits_into_plain_dir() {
            let dir = tempdir().expect("tempdir");
            let temp = dir.path().join(".payload.AbC123");
            let dest = dir.path().join("payload.bin");
            {
                let mut f = create_new_no_follow(&temp).expect("create temp");
                f.write_all(b"anchored commit").expect("write");
                f.flush().expect("flush");
            }

            rename_no_follow(&temp, &dest, true).expect("anchored rename");

            assert!(!temp.exists(), "temp file must be gone after rename");
            assert_eq!(std::fs::read(&dest).expect("read dest"), b"anchored commit");
        }

        /// `ReplaceIfExists = true` overwrites an existing destination, matching
        /// upstream `do_rename`.
        #[test]
        fn rename_no_follow_replaces_existing() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("existing.bin");
            std::fs::write(&dest, b"old").expect("seed dest");
            let temp = dir.path().join(".existing.XyZ789");
            {
                let mut f = create_new_no_follow(&temp).expect("create temp");
                f.write_all(b"new").expect("write");
            }

            rename_no_follow(&temp, &dest, true).expect("replace");
            assert_eq!(std::fs::read(&dest).expect("read"), b"new");
        }

        /// `create_new_no_follow` fails with `AlreadyExists` when the name is
        /// taken, preserving the `CREATE_NEW` retry contract of the temp-name
        /// loop.
        #[test]
        fn create_new_no_follow_rejects_existing_name() {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join(".taken.Aa0000");
            let _first = create_new_no_follow(&path).expect("first create");
            let err = create_new_no_follow(&path).expect_err("second must fail");
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        }

        /// Anchoring proof (the CVE-2024-12747 residual): when the destination
        /// parent is swapped for a directory reparse point (junction) pointing
        /// at an attacker-controlled tree, the anchored commit must refuse to
        /// follow it - the attacker directory must never receive the file.
        ///
        /// Junctions are created without privilege via
        /// `create_directory_symlink_or_junction`, so this runs on unprivileged
        /// CI. If even the junction fallback is unavailable the test skips.
        #[test]
        fn rename_no_follow_refuses_reparse_point_parent() {
            let root = tempdir().expect("tempdir");
            let real_dest = root.path().join("real_dest");
            let attacker = root.path().join("attacker");
            std::fs::create_dir(&real_dest).expect("real_dest");
            std::fs::create_dir(&attacker).expect("attacker");
            // A sentinel proving the attacker tree is untouched.
            std::fs::write(attacker.join("keep.txt"), b"keep").expect("sentinel");

            // Temp source lives in the root, outside the swapped directory.
            let temp = root.path().join(".loot.Zz9999");
            {
                let mut f = create_new_no_follow(&temp).expect("create temp");
                f.write_all(b"loot").expect("write");
            }

            // Swap: move the real destination aside and plant a junction at its
            // path pointing at the attacker tree.
            let aside = root.path().join("real_dest.aside");
            std::fs::rename(&real_dest, &aside).expect("move aside");
            match crate::win_symlink::create_directory_symlink_or_junction(&attacker, &real_dest) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("skipping: cannot create reparse point ({err})");
                    return;
                }
            }

            let dest = real_dest.join("victim.bin");
            let result = rename_no_follow(&temp, &dest, true);

            assert!(
                result.is_err(),
                "anchored rename must refuse a reparse-point destination parent"
            );
            assert!(
                !attacker.join("victim.bin").exists(),
                "attacker tree must never receive the committed file"
            );
            assert!(
                attacker.join("keep.txt").exists(),
                "attacker sentinel must be untouched"
            );
        }
    }
}

#[cfg(windows)]
pub use imp::{create_new_no_follow, rename_no_follow};

/// Returns `true` when an I/O error represents a cross-device link (`EXDEV`).
///
/// This is the single source of truth for the EXDEV detection every
/// temp->destination commit path shares. A temp file staged in a `--temp-dir`
/// (or partial dir) that lives on a different filesystem than the destination
/// cannot be `rename(2)`d across the mount, so the commit falls back to
/// copy+remove (upstream `util1.c:robust_rename()`).
///
/// The check is keyed on the raw OS error number rather than
/// [`std::io::ErrorKind::CrossesDevices`] so it stays robust across std
/// releases and is byte-identical on both sides of the commit divergence it
/// unifies (the engine local-copy guard and the transfer disk-commit path):
///
/// - Unix: `raw_os_error() == libc::EXDEV` (errno 18).
/// - Windows: `raw_os_error() == 17` (`ERROR_NOT_SAME_DEVICE`).
///
/// Every other platform reports `false`, so callers surface the original error.
#[must_use]
pub fn is_cross_device(error: &std::io::Error) -> bool {
    match error.raw_os_error() {
        #[cfg(unix)]
        Some(code) => code == libc::EXDEV,
        #[cfg(windows)]
        Some(code) => code == 17, // ERROR_NOT_SAME_DEVICE
        #[cfg(not(any(unix, windows)))]
        Some(_) => false,
        None => false,
    }
}

/// True for the transient Win32 error numbers a concurrent commit to the *same*
/// destination produces while another committer briefly holds the no-follow pin
/// on the destination parent.
///
/// [`imp::rename_no_follow`] pins that parent without `FILE_SHARE_DELETE` (the
/// reparse-swap hardening), so a second committer racing to replace the same
/// destination can be denied for the moment the pin is held:
///
/// - `ERROR_SHARING_VIOLATION` (32)
/// - `ERROR_ACCESS_DENIED` (5)
///
/// Both clear as soon as the other committer's handle drops, so the commit
/// retries. Every other error number is terminal and must surface unchanged -
/// notably `ERROR_NOT_SAME_DEVICE` (17, EXDEV) so the caller falls back to
/// copy+remove, `ERROR_FILENAME_EXCED_RANGE` (206), and `ERROR_FILE_NOT_FOUND`
/// (2).
///
/// oc-specific robustness: upstream rsync never commits two temp files to one
/// destination concurrently, so this races only in the engine's parallel
/// `DestinationWriteGuard` path, never on the wire.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_transient_rename_contention(raw_os_error: i32) -> bool {
    // ERROR_ACCESS_DENIED = 5, ERROR_SHARING_VIOLATION = 32.
    matches!(raw_os_error, 5 | 32)
}

#[cfg(test)]
mod contention_tests {
    use super::is_transient_rename_contention;

    /// The two transient contention codes retry; unrelated codes - including
    /// EXDEV (17), which must fall back to copy+remove - stay terminal.
    #[test]
    fn only_sharing_and_access_denied_are_transient() {
        assert!(is_transient_rename_contention(32)); // ERROR_SHARING_VIOLATION
        assert!(is_transient_rename_contention(5)); // ERROR_ACCESS_DENIED
        assert!(!is_transient_rename_contention(2)); // ERROR_FILE_NOT_FOUND
        assert!(!is_transient_rename_contention(17)); // ERROR_NOT_SAME_DEVICE (EXDEV)
        assert!(!is_transient_rename_contention(206)); // ERROR_FILENAME_EXCED_RANGE
        assert!(!is_transient_rename_contention(0));
    }
}
