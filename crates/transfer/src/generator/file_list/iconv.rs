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

use protocol::flist::DualFileList;

use super::super::GeneratorContext;
use super::super::io_error_flags;

impl GeneratorContext {
    /// Drops file-list entries whose names cannot be strictly transcoded to
    /// the remote charset under `--iconv`, emitting the upstream diagnostic and
    /// recording `IOERR_GENERAL` (exit 23) for each.
    ///
    /// Runs after the filesystem walk populates `file_list`/`source_bases` and
    /// before the sort, so dropped entries never receive an ndx. A directory
    /// whose own name is unconvertible is dropped here too; its children carry
    /// the same unconvertible bytes as a path prefix and are dropped alongside
    /// it, keeping the surviving list self-consistent.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1631` `send_file1()` - `rprintf(FERROR_XFER, "[%s] cannot
    ///   convert filename: %s (%s)\n", ...)` then `return NULL`.
    pub(super) fn drop_unconvertible_entries(&mut self) {
        let Some(converter) = self.config.connection.iconv.clone() else {
            return;
        };
        if converter.is_identity() {
            return;
        }

        let entries = std::mem::take(&mut self.file_list).into_vec();
        let bases = std::mem::take(&mut self.source_bases);
        self.file_list = DualFileList::with_capacity(entries.len());
        self.source_bases = Vec::with_capacity(entries.len());

        let mut any_dropped = false;
        // Re-push surviving (entry, base) pairs directly to preserve the already
        // interned source base; going through `push_file_item` would re-derive a
        // base by treating the stored base as a full path, which is wrong.
        for (entry, base) in entries.into_iter().zip(bases) {
            let convertible = {
                let name = entry.name_bytes();
                let ok = converter.local_to_remote(&name).is_ok();
                if !ok {
                    // upstream: flist.c:1631 - unconditional FERROR_XFER message.
                    eprintln!(
                        "{}",
                        protocol::iconv::cannot_convert_filename_message("sender", &name)
                    );
                }
                ok
            };
            if convertible {
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
}
