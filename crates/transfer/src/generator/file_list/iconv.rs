//! Strict `--iconv` validation of built file-list entries.
//!
//! Upstream rsync transcodes every filename through `iconvbufs(ic_send, ...,
//! ICB_INIT)` in `send_file1()`. `ICB_INIT` is the strict mode: a byte
//! sequence that cannot be represented in the peer charset makes the call
//! return `< 0`, whereupon upstream sets `io_error |= IOERR_GENERAL`, prints a
//! `cannot convert filename` diagnostic, and `return NULL`s so the entry never
//! enters the file list. Because the same flist backs both the wire and the
//! data phase, the drop must happen at build time - before ndx assignment and
//! INC_RECURSE segmentation - or sender/receiver ndx values would desync.
//!
//! # Upstream Reference
//!
//! - `flist.c:1614-1638` `send_file1()` - strict `ic_send` conversion + skip.
//! - `flist.c:757` `recv_file_entry()` - the receiver mirrors this.

use protocol::CompatibilityFlags;
use protocol::flist::DualFileList;
use protocol::flist::FileEntry;

use super::super::GeneratorContext;
use super::super::io_error_flags;

impl GeneratorContext {
    /// Drops file-list entries whose name - or, when `CF_SYMLINK_ICONV` was
    /// negotiated, symlink TARGET - cannot be strictly transcoded to the remote
    /// charset under `--iconv`, emitting the upstream diagnostic and recording
    /// `IOERR_GENERAL` (exit 23) for each.
    ///
    /// Runs after the filesystem walk populates `file_list`/`source_bases` and
    /// before the sort, so dropped entries never receive an ndx. A directory
    /// whose own name is unconvertible is dropped here too; its children carry
    /// the same unconvertible bytes as a path prefix and are dropped alongside
    /// it, keeping the surviving list self-consistent.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1631` `send_file1()` - name failure: `rprintf(FERROR_XFER,
    ///   "[%s] cannot convert filename: %s (%s)\n", ...)` then `return NULL`.
    /// - `flist.c:1642-1651` `send_file1()` - symlink-target failure (guarded by
    ///   `symlink_len && sender_symlink_iconv`): `rprintf(FERROR_XFER, "[%s]
    ///   cannot convert symlink data for: %s (%s)\n", ...)` then `return NULL`.
    pub(super) fn drop_unconvertible_entries(&mut self) {
        let Some(converter) = self.config.connection.iconv.clone() else {
            return;
        };
        if converter.is_identity() {
            return;
        }

        // upstream: compat.c:765-767 - the symlink TARGET is only strict-checked
        // (and thus only a drop condition) when `sender_symlink_iconv` is set,
        // i.e. the peer negotiated CF_SYMLINK_ICONV. Otherwise targets ship as
        // raw local bytes and never gate list membership.
        let symlink_iconv = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::SYMLINK_ICONV));

        let entries = std::mem::take(&mut self.file_list).into_vec();
        let bases = std::mem::take(&mut self.source_bases);
        self.file_list = DualFileList::with_capacity(entries.len());
        self.source_bases = Vec::with_capacity(entries.len());

        let mut any_dropped = false;
        // Re-push surviving (entry, base) pairs directly to preserve the already
        // interned source base; going through `push_file_item` would re-derive a
        // base by treating the stored base as a full path, which is wrong.
        for (entry, base) in entries.into_iter().zip(bases) {
            if Self::entry_is_convertible(&converter, symlink_iconv, &entry) {
                self.file_list.push(entry);
                self.source_bases.push(base);
            } else {
                any_dropped = true;
            }
        }

        if any_dropped {
            // upstream: flist.c:1633 - io_error |= IOERR_GENERAL -> exit 23.
            self.add_io_error(io_error_flags::IOERR_GENERAL);
        }
    }

    /// Returns whether `entry` survives strict `--iconv` transcoding, emitting
    /// the matching upstream `FERROR_XFER` diagnostic when it does not.
    ///
    /// The name is checked first (upstream `send_file1` returns before ever
    /// reaching the symlink block on a name failure), so at most one message is
    /// printed per entry, matching upstream.
    fn entry_is_convertible(
        converter: &protocol::iconv::FilenameConverter,
        symlink_iconv: bool,
        entry: &FileEntry,
    ) -> bool {
        let name = entry.name_bytes();
        if converter.local_to_remote(&name).is_err() {
            // upstream: flist.c:1631 - FERROR_XFER "cannot convert filename".
            eprintln!(
                "{}",
                protocol::iconv::cannot_convert_filename_message("sender", &name)
            );
            return false;
        }

        // upstream: flist.c:1642 - `if (symlink_len && sender_symlink_iconv)`.
        if symlink_iconv
            && entry.is_symlink()
            && let Some(target) = entry.link_target()
        {
            let target_bytes = symlink_target_bytes(target);
            if converter.local_to_remote(&target_bytes).is_err() {
                // upstream: flist.c:1648 - FERROR_XFER "cannot convert symlink
                // data for", keyed by the symlink's own path.
                eprintln!(
                    "{}",
                    protocol::iconv::cannot_convert_symlink_message("sender", &name)
                );
                return false;
            }
        }

        true
    }
}

/// Returns the local-charset bytes of a symlink target for strict-convertibility
/// testing. Mirrors the bytes the flist writer feeds to `ic_send`: the
/// wire-form separator normalisation (`\` -> `/`) applied by the writer is
/// separator-only and cannot change strict convertibility, so the raw local
/// bytes are tested directly.
#[cfg(unix)]
fn symlink_target_bytes(target: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    std::borrow::Cow::Borrowed(target.as_os_str().as_bytes())
}

/// Non-Unix variant: `OsStr` bytes are WTF-8; the flist writer emits the same
/// bytes (after `\` -> `/` normalisation, which is convertibility-neutral).
#[cfg(not(unix))]
fn symlink_target_bytes(target: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    std::borrow::Cow::Borrowed(target.as_os_str().as_encoded_bytes())
}

#[cfg(all(test, feature = "iconv", unix))]
mod drop_decision_tests {
    use super::*;
    use protocol::flist::FileEntry;
    use protocol::iconv::FilenameConverter;

    /// local=UTF-8, remote=ISO-8859-1: a `あ` (U+3042) byte sequence has no
    /// Latin-1 / windows-1252 representation, so `local_to_remote` fails
    /// strictly. (`€` is avoided because encoding_rs maps the ISO-8859-1 label
    /// to windows-1252, which DOES contain `€` at 0x80.)
    fn latin1_converter() -> FilenameConverter {
        FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter builds")
    }

    /// upstream: flist.c:1642-1651 - with `sender_symlink_iconv` (CF_SYMLINK_ICONV
    /// negotiated) an unconvertible symlink TARGET makes `send_file1` `return
    /// NULL`, dropping the entry. The name here converts fine, so the target is
    /// the sole drop cause.
    #[test]
    fn symlink_with_unconvertible_target_dropped_when_negotiated() {
        let conv = latin1_converter();
        let entry = FileEntry::new_symlink("link".into(), "あ".into());
        assert!(!GeneratorContext::entry_is_convertible(&conv, true, &entry));
    }

    /// Without CF_SYMLINK_ICONV the target ships as raw local bytes and never
    /// gates list membership - the entry survives even with a non-Latin-1 target.
    ///
    /// upstream: flist.c:1642 gate `sender_symlink_iconv` is 0.
    #[test]
    fn symlink_with_unconvertible_target_kept_when_not_negotiated() {
        let conv = latin1_converter();
        let entry = FileEntry::new_symlink("link".into(), "あ".into());
        assert!(GeneratorContext::entry_is_convertible(&conv, false, &entry));
    }

    /// An unconvertible NAME is always a drop cause, independent of the symlink
    /// gate. upstream: flist.c:1631 `return NULL`.
    #[test]
    fn unconvertible_name_always_dropped() {
        let conv = latin1_converter();
        let entry = FileEntry::new_file("あ".into(), 0, 0o100_644);
        assert!(!GeneratorContext::entry_is_convertible(
            &conv, false, &entry
        ));
    }

    /// A fully convertible symlink survives even with the gate on.
    #[test]
    fn convertible_symlink_survives_with_gate_on() {
        let conv = latin1_converter();
        let entry = FileEntry::new_symlink("link".into(), "sub/target".into());
        assert!(GeneratorContext::entry_is_convertible(&conv, true, &entry));
    }
}
