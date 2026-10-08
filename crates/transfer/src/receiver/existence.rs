//! The receiver side of the `--existing` / `--ignore-existing` gate.
//!
//! Every file type consults the one predicate in `engine::existence_gate`,
//! so regular files, directories, symlinks, devices and specials skip under
//! exactly the same conditions.
//!
//! upstream: generator.c:1757-1806 recv_generator()

use std::fs;
use std::path::{Path, PathBuf};

use engine::existence_gate::{DestinationEntry, ExistenceSkip, existence_skip};
use metadata::metadata_unchanged;
use protocol::flist::FileEntry;

use super::ReceiverContext;

impl ReceiverContext {
    /// Decides whether the existence gate skips `entry`, given the destination
    /// as the pre-transfer `lstat` saw it.
    pub(in crate::receiver) const fn existence_skip(
        &self,
        entry: &FileEntry,
        destination: DestinationEntry,
    ) -> Option<ExistenceSkip> {
        existence_skip(
            self.config.file_selection.existing_only,
            self.config.file_selection.ignore_existing,
            entry.is_dir(),
            destination,
        )
    }

    /// Applies the existence gate to a non-regular entry (directory, symlink,
    /// device or special) at `dest_path` and emits the SKIP notice, returning
    /// `true` when the entry is skipped.
    ///
    /// upstream: generator.c:1745 gen_entry_stat() - the destination is read
    /// with `link_stat()`, so a destination symlink counts as present even when
    /// it dangles; only a directory under `--keep-dirlinks` follows it.
    pub(in crate::receiver) fn skip_non_regular_by_existence_gate<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        dest_dir: &Path,
        dest_path: &Path,
    ) -> bool {
        if !self.config.file_selection.existing_only && !self.config.file_selection.ignore_existing
        {
            return false;
        }
        let mut stat = fs::symlink_metadata(dest_path);
        if entry.is_dir()
            && self.config.flags.keep_dirlinks
            && stat
                .as_ref()
                .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            stat = fs::metadata(dest_path);
        }
        let Some(skip) = self.existence_skip(entry, DestinationEntry::from_lstat(&stat)) else {
            return false;
        };
        self.emit_existence_skip_notice(writer, entry, dest_dir, skip, || {
            stat.as_ref().map_or("", |meta| {
                non_regular_exists_reason(self, entry, dest_path, meta)
            })
        });
        true
    }

    /// Emits upstream's SKIP notice for an entry the existence gate skipped.
    ///
    /// upstream: generator.c:1767-1771 - `not creating new %s "%s"` names the
    /// entry a "directory" or a "file"; generator.c:1785-1799 - `%s exists%s`
    /// gains a parenthesised reason at `INFO_GTE(SKIP, 2)`. Both are gated on
    /// `INFO_GTE(SKIP, 1)`. A skipped directory becomes `skip_dir`
    /// (generator.c:1762), so nothing below it is reported; its descendants are
    /// recognised by their absent parent.
    pub(in crate::receiver) fn emit_existence_skip_notice<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        dest_dir: &Path,
        skip: ExistenceSkip,
        exists_reason: impl FnOnce() -> &'static str,
    ) {
        if !logging::info_gte(logging::InfoFlag::Skip, 1) {
            return;
        }
        let name = entry.path().to_string_lossy();
        let line = match skip {
            ExistenceSkip::NotCreatingNew => {
                if !parent_present(dest_dir, entry.path()) {
                    return;
                }
                let kind = if entry.is_dir() { "directory" } else { "file" };
                format!("not creating new {kind} \"{name}\"\n")
            }
            ExistenceSkip::Exists => {
                let reason = if logging::info_gte(logging::InfoFlag::Skip, 2) {
                    exists_reason()
                } else {
                    ""
                };
                format!("{name} exists{reason}\n")
            }
        };
        let _ = self.emit_info_line(writer, &line);
    }
}

/// Reports whether the destination parent of `relative` exists, i.e. whether
/// the entry lies outside a directory the gate already skipped.
fn parent_present(dest_dir: &Path, relative: &Path) -> bool {
    relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .is_none_or(|parent| fs::symlink_metadata(dest_dir.join(parent)).is_ok())
}

/// The `INFO_GTE(SKIP, 2)` reason for a non-regular entry left standing by
/// `--ignore-existing`.
///
/// upstream: generator.c:1788-1797 - `(type change)` when the `FT_*` class
/// differs, `(file change)` when `quick_check_ok()` fails (another symlink
/// target, device rdev or special `S_IFMT`), `(attr change)` when
/// `unchanged_attrs()` fails, else `(uptodate)`.
fn non_regular_exists_reason(
    context: &ReceiverContext,
    entry: &FileEntry,
    dest_path: &Path,
    meta: &fs::Metadata,
) -> &'static str {
    let Some(same_content) = same_class_content(entry, dest_path, meta) else {
        return " (type change)";
    };
    if !same_content {
        return " (file change)";
    }
    // upstream: generator.c:476-488 unchanged_attrs() - a symlink compares
    // its mtime only when link times are kept, and never its permissions
    // (Linux lacks CAN_CHMOD_SYMLINK).
    let mut options = context.build_metadata_options();
    if entry.is_symlink() {
        options = options
            .preserve_permissions(false)
            .preserve_times(context.config.flags.times && !context.config.flags.omit_link_times);
    }
    if metadata_unchanged(
        entry,
        &options,
        meta,
        context.config.file_selection.modify_window,
    ) {
        " (uptodate)"
    } else {
        " (attr change)"
    }
}

/// Compares `entry` against the destination `meta`: `None` when the `FT_*`
/// class differs (generator.c:614 get_file_type()), otherwise whether
/// `quick_check_ok()` (generator.c:630-683) holds.
fn same_class_content(entry: &FileEntry, dest_path: &Path, meta: &fs::Metadata) -> Option<bool> {
    let dest = meta.file_type();
    if entry.is_dir() {
        dest.is_dir().then_some(true)
    } else if entry.is_symlink() {
        dest.is_symlink().then(|| {
            fs::read_link(dest_path).ok().as_deref() == entry.link_target().map(PathBuf::as_path)
        })
    } else {
        special_content_matches(entry, meta)
    }
}

#[cfg(unix)]
fn special_content_matches(entry: &FileEntry, meta: &fs::Metadata) -> Option<bool> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    const S_IFMT: u32 = 0o170_000;
    let dest = meta.file_type();
    if entry.is_device() {
        (dest.is_block_device() || dest.is_char_device()).then(|| {
            meta.rdev()
                == metadata::device_word(
                    entry.rdev_major().unwrap_or(0),
                    entry.rdev_minor().unwrap_or(0),
                )
        })
    } else {
        (dest.is_fifo() || dest.is_socket()).then(|| meta.mode() & S_IFMT == entry.mode() & S_IFMT)
    }
}

#[cfg(not(unix))]
fn special_content_matches(_entry: &FileEntry, _meta: &fs::Metadata) -> Option<bool> {
    None
}
