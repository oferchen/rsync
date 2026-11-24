//! The generator's parent-directory check on a list without INC_RECURSE.
//!
//! upstream: generator.c:1730-1752 recv_generator() - without INC_RECURSE every
//! entry's parent directory must itself be in the file list, or the list is
//! bad: "ABORTING due to invalid path from sender: %s/%s" and
//! `exit_cleanup(RERR_PROTOCOL)`. The top-level "." needs no parent (the
//! `*dn != '.' || dn[1]` guard). Under `--delete-missing-args` a missing-arg
//! entry (mode 0) may name a parent the list lacks: that is reported with
//! "WARNING: parent dir is absent in the file list: %s" and allowed, and a
//! parent that is itself a missing-arg entry counts as present. Under
//! INC_RECURSE the read-time check in `receive.rs` (flist.c:3227-3245) applies
//! instead.
//!
//! Deviation, on purpose: upstream runs the check as recv_generator() reaches
//! each entry, so when it aborts it has already run `--delete-before` and
//! created the directories that sort before the bad entry. This receiver
//! generates in passes, so the check runs once over the sorted list before
//! the first of them and aborts with the destination untouched.
//!
//! It skips what upstream's walk returns early for before the check:
//! `--list-only` (generator.c:1675-1681), a dry run into a destination it did
//! not create (generator.c:1716, `dry_run > 1`), an entry below a directory a
//! dry run found missing (generator.c:1883, `dry_missing_dir`), an entry the
//! daemon's filter
//! refuses (generator.c:1698-1712) and an entry below a listed directory that
//! filter refused (generator.c:1684-1695, `skip_dir`).

use std::cmp::Ordering;
use std::io;
use std::path::Path;
use std::sync::Arc;

use protocol::CompatibilityFlags;
use protocol::flist::{
    FileEntry, compare_file_entries, compare_file_entries_pre29, flist_find_name,
};

use super::strip_leading_slashes;
use crate::receiver::ReceiverContext;
use crate::receiver::daemon_filter::DaemonFilterGate;
use crate::receiver::dest_root::DestRoot;

impl ReceiverContext {
    /// Aborts when an entry's parent directory is not in the file list.
    ///
    /// Each entry costs one comparison of its parent against the previous
    /// entry's; the list is searched (`O(log n)`) only when the parent
    /// changes, as upstream caches `parent_dirname`. Nothing is allocated
    /// unless the list is refused or a warning is sent.
    ///
    /// # Errors
    ///
    /// A generator-raised [`protocol::ProtocolViolation`] (exit 2) naming the
    /// entry, or the writer's error when the `--delete-missing-args` warning
    /// cannot be sent.
    pub(in crate::receiver) fn abort_on_entry_without_parent<W>(
        &self,
        dest_dir: &Path,
        writer: &mut W,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        if inc_recurse || self.config.flags.list_only || self.dest_root == DestRoot::DryRunAbsent {
            return Ok(());
        }
        let pre29 = self.protocol.as_u8() < 29;
        let list = self.file_list.live();
        // Upstream searches `cur_flist->sorted`. Under `--iconv` this list
        // stays in wire order (receive.rs), so search a sorted view of it.
        let sorted_view: Option<Vec<&FileEntry>> = self.iconv_reorder_suppressed().then(|| {
            let mut view: Vec<&FileEntry> = list.iter().filter(|e| e.is_active()).collect();
            view.sort_by(|a, b| sort_order(a, b, pre29));
            view
        });
        let find = |name: &Path| match &sorted_view {
            Some(view) => flist_find_name(view, name, pre29),
            None => flist_find_name(list, name, pre29),
        };
        let delete_missing_args = self.config.file_selection.delete_missing_args;
        let mut daemon_filters = self.daemon_filter_gate(dest_dir);
        // The previous entry's interned dirname, its stripped form and what
        // the lookup found. Entries of one directory share the dirname `Arc`,
        // so the common case costs one pointer compare (upstream compares the
        // `parent_dirname` pointer first, generator.c:1731).
        let mut parent: Option<(&Arc<Path>, &Path, Option<&FileEntry>)> = None;
        // upstream: generator.c:1716 and :1883 - under --dry-run a directory
        // missing from the destination becomes `dry_missing_dir`, and entries
        // below it skip the check until the walk leaves it (:1730).
        let dry_run = self.config.flags.dry_run;
        let implied_dirs_missing = self.config.flags.relative
            && self.config.flags.no_implied_dirs
            && self.protocol.as_u8() < 30;
        let mut dry_missing: Option<&FileEntry> = None;
        for entry in list.iter().filter(|e| e.is_active()) {
            if dry_run {
                if dry_missing.is_some_and(|dir| is_below(entry, dir, implied_dirs_missing)) {
                    parent = None;
                    continue;
                }
                dry_missing = (entry.is_dir()
                    && dest_dir.join(entry.path()).symlink_metadata().is_err())
                .then_some(entry);
            }
            let dirname = entry.dirname();
            let (dn, found) = match parent {
                // upstream: generator.c:1731 - `parent_dirname != dn` compares the
                // pointer before strcmp().
                Some((cached, dn, found)) if Arc::ptr_eq(cached, dirname) => {
                    if found.is_some_and(FileEntry::is_dir) {
                        continue;
                    }
                    (dn, found)
                }
                _ => {
                    let dn = strip_leading_slashes(dirname);
                    let found = match parent {
                        Some((_, cached, found)) if cached.as_os_str() == dn.as_os_str() => found,
                        _ if dn.as_os_str().is_empty() || dn.as_os_str() == "." => None,
                        _ => find(dn),
                    };
                    parent = Some((dirname, dn, found));
                    (dn, found)
                }
            };
            if dn.as_os_str().is_empty() || dn.as_os_str() == "." {
                continue;
            }
            let missing_arg = delete_missing_args && entry.mode() == 0;
            if found.is_some_and(|f| f.is_dir() || (missing_arg && f.mode() == 0)) {
                continue;
            }
            if daemon_filters
                .as_mut()
                .is_some_and(|filters| daemon_skips(filters, entry, &find))
            {
                continue;
            }
            if missing_arg && found.is_none() {
                self.emit_error_line(
                    writer,
                    &format!(
                        "WARNING: parent dir is absent in the file list: {}\n",
                        dn.display()
                    ),
                )?;
                continue;
            }
            let basename = Path::new(entry.path().file_name().unwrap_or_default());
            return Err(protocol::generator_protocol_violation(format!(
                "ABORTING due to invalid path from sender: {}/{}",
                dn.display(),
                basename.display()
            )));
        }
        Ok(())
    }
}

/// Whether upstream's walk returns before the parent check: the daemon's
/// filter refuses the entry (generator.c:1698-1712), or a listed directory
/// above it was refused, so `skip_dir` covers it (generator.c:1684-1695).
fn daemon_skips<'a>(
    filters: &mut DaemonFilterGate,
    entry: &FileEntry,
    find: &impl Fn(&Path) -> Option<&'a FileEntry>,
) -> bool {
    let name = entry.path();
    !filters.allows(name, entry.is_dir())
        || name
            .ancestors()
            .skip(1)
            .take_while(|a| !a.as_os_str().is_empty())
            .any(|a| find(a).is_some_and(FileEntry::is_dir) && !filters.allows(a, true))
}

/// upstream: generator.c:1403 `is_below()` - deeper than `dir`, and when
/// implied directories are missing (`-R --no-implied-dirs` below protocol 30)
/// also under its name.
fn is_below(entry: &FileEntry, dir: &FileEntry, implied_dirs_missing: bool) -> bool {
    let (name, dir_name) = (entry.name_bytes(), dir.name_bytes());
    depth(&name) > depth(&dir_name)
        && (!implied_dirs_missing
            || name
                .strip_prefix(&dir_name[..])
                .is_some_and(|rest| rest.first() == Some(&b'/')))
}

/// upstream: `F_DEPTH` - the number of path components. Only the relative
/// depth of two listed names is compared, so the top-level "." needs no case
/// of its own: it is never missing on a dry run that reaches this walk.
fn depth(name: &[u8]) -> usize {
    name.iter().filter(|&&b| b == b'/').count() + 1
}

/// The order the receiver sorts its list in (flist.c:3814 `f_name_cmp`).
fn sort_order(a: &FileEntry, b: &FileEntry, pre29: bool) -> Ordering {
    if pre29 {
        compare_file_entries_pre29(a, b)
    } else {
        compare_file_entries(a, b)
    }
}
