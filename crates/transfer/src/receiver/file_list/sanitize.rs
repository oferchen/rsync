//! Receiver-side file-list sanity validation.
//!
//! When `trust_sender` is false, the receiver validates each entry to prevent
//! directory-traversal attacks from a malicious sender. Absolute paths, `..`
//! components, and (on Windows) drive/UNC prefixes are stripped from the
//! file list before any disk operation runs against them.

use logging::info_log;

use super::super::ReceiverContext;
use super::super::quick_check::path_contains_dot_dot;

impl ReceiverContext {
    /// Sanitizes the received file list by removing entries with unsafe paths.
    ///
    /// When `trust_sender` is false, the receiver validates each entry to prevent
    /// directory traversal attacks from a malicious sender:
    ///
    /// - Entries with absolute paths are rejected (unless `--relative` is active)
    /// - Entries containing `..` path components are rejected
    /// - Symlink entries pointing outside the transfer tree are rejected
    ///
    /// Rejected entries are removed from the file list and warnings are emitted.
    /// Returns the number of entries removed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:769`: `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
    /// - `options.c:2595`: `trust_sender_args = trust_sender_filter = 1`
    pub(in crate::receiver) fn sanitize_file_list(&mut self) -> usize {
        let relative_paths = self.config.flags.relative;

        let removed = if self.config.trust_sender {
            0
        } else {
            let original_len = self.file_list.len();

            self.file_list.retain(|entry| {
                let path = entry.path();

                // Check for absolute paths (reject unless --relative is active).
                // upstream: flist.c:769 `!relative_paths && *thisname == '/'`
                if !relative_paths && path.has_root() {
                    info_log!(
                        Misc,
                        1,
                        "ERROR: rejecting file-list entry with absolute path from sender: {} {}{}",
                        path.display(),
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    );
                    return false;
                }

                // Windows-only: reject any path that carries a Component::Prefix
                // (drive letter, UNC, `\\?\`, `\\.\`). `Path::has_root()` is
                // false for drive-relative inputs such as `C:foo`, but joining
                // such a path onto `dest_dir` on Windows discards `dest_dir`
                // entirely (`Path::join` semantics), letting a malicious sender
                // escape the destination tree. Upstream rsync runs only under
                // Cygwin's POSIX layer where these forms cannot occur, so the
                // defense lives only on the native-Win32 build. The `--relative`
                // exemption above does not apply: drive prefixes are never
                // valid in a wire path.
                #[cfg(windows)]
                if path
                    .components()
                    .next()
                    .is_some_and(|c| matches!(c, std::path::Component::Prefix(_)))
                {
                    info_log!(
                        Misc,
                        1,
                        "ERROR: rejecting file-list entry with Windows drive or UNC prefix from sender: {} {}{}",
                        path.display(),
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    );
                    return false;
                }

                // Check for `..` path components (always rejected).
                // upstream: flist.c:769 `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS) < 0`
                if path_contains_dot_dot(path) {
                    info_log!(
                        Misc,
                        1,
                        "ERROR: rejecting file-list entry with \"..\" component from sender: {} {}{}",
                        path.display(),
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    );
                    return false;
                }

                true
            });

            original_len - self.file_list.len()
        };

        // upstream: flist.c:3106-3119 - strip_root in flist_sort_and_clean()
        // Runs unconditionally: leading-slash stripping is a functional
        // requirement for --relative mode, not a security check.
        if relative_paths {
            for entry in &mut self.file_list {
                if entry.path().has_root() {
                    entry.strip_leading_slashes();
                }
            }
        }

        removed
    }
}
