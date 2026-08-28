//! Cross-platform extended-attribute glue.
//!
//! Sits above the platform backends and reproduces upstream rsync's xattr
//! flow:
//!
//! - On Unix the [`xattr_unix`](crate::xattr_unix) module wraps the
//!   `xattr` crate (`get`/`set`/`list`/`remove`).
//! - On Windows the `xattr_windows` module
//!   maps every named xattr onto an NTFS Alternate Data Stream
//!   (`path:name:$DATA`) so the client/daemon surface remains the same.
//!
//! The per-attribute primitives all take and return raw byte slices for
//! attribute names so the wire format (which is byte-oriented) and the
//! POSIX/NTFS native encodings can both be expressed without lossy
//! conversions in this layer.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:rsync_xal_get()` - read xattrs into the wire-list cache.
//! - `xattrs.c:rsync_xal_set()` - apply received xattrs on the receiver.
//! - `xattrs.c:64-68, 254-257` - permitted-namespace policy on Linux.

use crate::error::MetadataError;
use crate::xattr_send::XattrSendOptions;
use crate::xattr_send::XattrSyncFilters;
use protocol::xattr::XattrList;
use std::collections::HashSet;
use std::io;
use std::path::Path;

#[cfg(unix)]
use crate::xattr_unix as backend;
#[cfg(windows)]
use crate::xattr_windows as backend;

/// Returns upstream's `user_only` value for the receiver/local xattr paths.
///
/// upstream: xattrs.c:249 `int user_only = am_sender ? 0 : !am_root;` and
/// xattrs.c:1002 `int user_only = am_root <= 0;` - the sender never restricts
/// to the user namespace (see
/// [`XattrRole::user_only`](crate::xattr_send::XattrRole::user_only)); every
/// receiver-side or local-copy path restricts a non-root process to the
/// `user.*` namespace, matching upstream's non-root receiver behaviour
/// (`xattrs.c:876` allows a non-root process to store only the user namespace).
///
/// The privilege test goes through [`crate::am_root`], which reads the cached
/// **libc** `geteuid` (`identity::is_root`), never a `rustix` raw syscall.
/// Upstream's `am_root` comes from `MY_UID()` (`rsync.h:1455` = libc
/// `geteuid()`, sampled at `main.c:1844`), so `fakeroot`'s LD_PRELOAD
/// interposition is visible to it. A raw syscall reports the real
/// unprivileged uid under `fakeroot` and would confine the receiver to
/// `user.*` in exactly the runs where upstream keeps `security.*`/`trusted.*`.
fn receiver_user_only() -> bool {
    #[cfg(target_os = "linux")]
    {
        !crate::am_root()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Checks whether an xattr name is permitted for the given `user_only` mode.
///
/// Mirrors upstream rsync `xattrs.c:256` in `rsync_xal_get()`:
/// `user_only ? !HAS_PREFIX(name, USER_PREFIX) : HAS_PREFIX(name, SYSTEM_PREFIX)`.
/// - `user_only == false` (the sender, root or not; and root receivers): keep
///   every namespace except `system.*`, so `user.*`, `security.*` (e.g. SELinux
///   labels), and `trusted.*` are all transmitted.
/// - `user_only == true` (a non-root receiver): keep only the `user.*` namespace,
///   matching the non-root store restriction at `xattrs.c:830`.
/// - On non-Linux platforms (macOS, FreeBSD, Windows): no namespace filtering,
///   since those systems use a single flat namespace (NTFS ADS, `com.apple.*`,
///   FreeBSD `user`-only, etc.).
#[cfg(target_os = "linux")]
fn is_xattr_permitted(name: &str, user_only: bool) -> bool {
    const USER_PREFIX: &str = "user.";
    const SYSTEM_PREFIX: &str = "system.";

    // upstream: xattrs.c:256 rsync_xal_get() - the sender (user_only == false)
    // skips only the system.* namespace, so a non-root sender still transmits
    // user.*, security.*, and trusted.*; a non-root receiver (user_only == true)
    // keeps only user.*.
    if user_only {
        name.starts_with(USER_PREFIX)
    } else {
        !name.starts_with(SYSTEM_PREFIX)
    }
}

/// On non-Linux platforms (macOS, FreeBSD, Windows), all xattr names are permitted.
#[cfg(not(target_os = "linux"))]
fn is_xattr_permitted(_name: &str, _user_only: bool) -> bool {
    true
}

fn map_xattr_error(context: &'static str, path: &Path, error: io::Error) -> MetadataError {
    MetadataError::new(context, path, error)
}

/// How [`list_attributes`] screens namespaces.
///
/// Upstream evaluates the namespace rule only as the `else` of the
/// `x`-modifier filter rule, so the two are mutually exclusive
/// (`xattrs.c:250-257`).
#[derive(Clone, Copy)]
enum NamespaceScreen {
    /// Apply upstream's namespace rule with the given `user_only` value.
    Apply { user_only: bool },
    /// Skip the namespace rule: an `x`-modifier filter governs the selection
    /// instead, exactly as upstream's `saw_xattr_filter` branch does.
    Bypass,
}

/// Returns the byte-encoded xattr names present on `path`, screened per
/// `screen` by [`is_xattr_permitted`].
fn list_attributes(
    path: &Path,
    follow_symlinks: bool,
    screen: NamespaceScreen,
) -> Result<Vec<Vec<u8>>, MetadataError> {
    let attrs = backend::list_attributes(path, follow_symlinks)
        .map_err(|error| map_xattr_error("list extended attributes", path, error))?;
    Ok(attrs
        .into_iter()
        .map(|name| backend::os_name_to_bytes(&name))
        .filter(|bytes| match screen {
            NamespaceScreen::Apply { user_only } => {
                is_xattr_permitted(&String::from_utf8_lossy(bytes), user_only)
            }
            NamespaceScreen::Bypass => true,
        })
        .collect())
}

/// Selects the namespace screen for one read.
///
/// upstream: xattrs.c:250-257 (`rsync_xal_get`) and :375-382 (`copy_xattrs`) -
/// the namespace test is the `else` of the `saw_xattr_filter` branch, so an
/// `x`-modifier filter REPLACES it rather than stacking with it. Both readers
/// make that same decision, so the rule has a single owner.
const fn namespace_screen(filter_governs: bool, user_only: bool) -> NamespaceScreen {
    if filter_governs {
        NamespaceScreen::Bypass
    } else {
        NamespaceScreen::Apply { user_only }
    }
}

/// Shorthand for the receiver/local-copy namespace screen.
fn receiver_screen() -> NamespaceScreen {
    NamespaceScreen::Apply {
        user_only: receiver_user_only(),
    }
}

fn read_attribute(
    path: &Path,
    name: &[u8],
    follow_symlinks: bool,
) -> Result<Option<Vec<u8>>, MetadataError> {
    backend::read_attribute(path, name, follow_symlinks)
        .map_err(|error| map_xattr_error("read extended attribute", path, error))
}

fn write_attribute(
    path: &Path,
    name: &[u8],
    value: &[u8],
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    backend::write_attribute(path, name, value, follow_symlinks)
        .map_err(|error| map_xattr_error("write extended attribute", path, error))
}

fn remove_attribute(path: &Path, name: &[u8], follow_symlinks: bool) -> Result<(), MetadataError> {
    backend::remove_attribute(path, name, follow_symlinks)
        .map_err(|error| map_xattr_error("remove extended attribute", path, error))
}

/// Reads xattr data from a file and returns it as a wire-format `XattrList`.
///
/// Per attribute the selection reproduces `rsync_xal_get()` in upstream order:
///
/// 1. The `x`-modifier filter, if any, decides alone; otherwise the namespace
///    rule applies. The two are mutually exclusive (`xattrs.c:250-257`). The
///    namespace rule keys on `user_only` (`xattrs.c:237`), which is `0` for
///    the sender and `!am_root` for the generator.
/// 2. The `rsync.%FOO` internal-attribute gate (`xattrs.c:260-267`): a sender
///    below `-XX` drops them all, and `--fake-super` additionally drops the
///    three store attributes at any level.
/// 3. The surviving name is translated to wire format via `local_to_wire()`.
///
/// Entries are sorted alphabetically by wire name, matching upstream's qsort
/// after collection. All values are stored as full data - abbreviation
/// (checksum substitution for large values) is handled by the wire encoder at
/// send time.
///
/// # Upstream Reference
///
/// - `xattrs.c:rsync_xal_get()` - reads xattrs, sorts by name, assigns nums
/// - `xattrs.c:get_xattr()` - entry point called from `make_file()`
pub fn read_xattrs_for_wire(
    path: &Path,
    opts: &XattrSendOptions<'_>,
) -> Result<XattrList, MetadataError> {
    use protocol::xattr::{XattrEntry, is_fake_super_store_attr, is_rsync_internal, local_to_wire};

    // upstream: xattrs.c:249 - `user_only = am_sender ? 0 : !am_root`. The
    // sender skips only the system.* namespace and still transmits user.*,
    // security.* (e.g. SELinux labels), and trusted.*; a non-root generator
    // reading the destination to diff it keeps user.* alone, so a namespace it
    // could never store cannot flip the itemize `x` column.
    let screen = namespace_screen(opts.filter.is_some(), opts.user_only());
    let attrs = list_attributes(path, opts.follow_symlinks, screen)?;
    let mut entries = Vec::with_capacity(attrs.len());

    for name in &attrs {
        let name_str = String::from_utf8_lossy(name);

        // upstream: xattrs.c:250-252 - name_is_excluded(name, NAME_IS_XATTR,
        // ALL_FILTERS) drops the attribute before it is ever read.
        if let Some(filter) = opts.filter
            && !filter(&name_str)
        {
            continue;
        }

        // upstream: xattrs.c:260-267 - no rsync.%FOO attribute is copied
        // without two -X options, and the fake-super store attributes are
        // never copied while --fake-super is active (am_root < 0), because
        // they describe this side's store rather than the file's metadata.
        if is_rsync_internal(&name_str)
            && ((opts.role.is_sender() && opts.preserve_xattrs < 2)
                || (opts.fake_super && is_fake_super_store_attr(&name_str)))
        {
            continue;
        }

        // upstream: xattrs.c:509-528 - translate local name to wire format.
        // `system.*` survives for a root sender, and also whenever a filter is
        // present, since upstream then skips the namespace test entirely.
        let wire_name = match local_to_wire(name, opts.am_root || opts.filter.is_some()) {
            Some(n) => n,
            None => continue, // Filtered out (namespace not exposable)
        };

        let value = match read_attribute(path, name, opts.follow_symlinks)? {
            Some(v) => v,
            None => continue,
        };

        entries.push(XattrEntry::new(wire_name, value));
    }

    // upstream: xattrs.c:296-297 - qsort by name
    entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));

    // upstream: xattrs.c:297-299 - after the ascending qsort, upstream walks the
    // array backwards assigning `rxa->num = count` down to 1, which lands
    // num = sorted_position + 1 (first name gets 1, last gets count). The
    // receiver re-derives the same 1-based ascending num in receive order
    // (protocol::xattr::cache `for num in 1..=count`), and the abbreviated
    // (>MAX_FULL_DATUM) value request round-trip keys on num. This MUST be
    // ascending: a descending assignment makes the sender resolve a request
    // for num N to a different entry and return the wrong xattr's value.
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.set_num((i + 1) as u32);
    }

    Ok(XattrList::with_entries(entries))
}

/// Removes from `destination` every extended attribute that also exists on
/// `source`, mirroring upstream `-a` without `-X`: a freshly written
/// destination carries none of the source's xattrs.
///
/// This is the post-clone correction for the macOS `clonefile` fast path,
/// which copies all of the source's xattrs verbatim. When xattr preservation
/// is disabled the destination must look as if it had been `open()`ed and
/// written fresh, so the source-originated attributes are stripped. Only
/// names present on `source` are removed, so attributes the filesystem
/// applied to the new inode on its own (e.g. `com.apple.provenance`) are
/// left untouched. Removing only names confirmed present on `destination`
/// avoids spurious `ENOATTR` churn.
pub fn strip_source_xattrs(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    confine_root: Option<&Path>,
) -> Result<(), MetadataError> {
    let source_attrs = list_attributes(source, follow_symlinks, receiver_screen())?;
    if source_attrs.is_empty() {
        return Ok(());
    }
    let source_names: HashSet<Vec<u8>> = source_attrs.into_iter().collect();

    let dest = DestinationXattrs::open(destination, follow_symlinks, confine_root);
    for name in list_attributes(destination, follow_symlinks, receiver_screen())? {
        if source_names.contains(&name) {
            dest.remove(&name)?;
        }
    }
    Ok(())
}

/// Reports whether the transferable extended attributes of `a` and `b` match.
///
/// Mirrors upstream `xattrs.c:xattrs_differ()` (consumed by `unchanged_attrs`):
/// only the attributes that participate in an `-X` transfer are compared - the
/// rsync-internal `rsync.%*` channel and namespaces the current privilege level
/// cannot access are excluded (`list_attributes` already applies the namespace
/// filter). Used by the `--link-dest` match-level check to decide whether a
/// basis file's xattrs already equal the source's.
// upstream: xattrs.c xattrs_differ() via generator.c:468 unchanged_attrs()
pub fn xattrs_match(a: &Path, b: &Path, follow_symlinks: bool) -> Result<bool, MetadataError> {
    let mut a_map: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = std::collections::BTreeMap::new();
    for name in list_attributes(a, follow_symlinks, receiver_screen())? {
        if protocol::xattr::is_rsync_internal(&String::from_utf8_lossy(&name)) {
            continue;
        }
        if let Some(value) = read_attribute(a, &name, follow_symlinks)? {
            a_map.insert(name, value);
        }
    }

    let mut b_count = 0usize;
    for name in list_attributes(b, follow_symlinks, receiver_screen())? {
        if protocol::xattr::is_rsync_internal(&String::from_utf8_lossy(&name)) {
            continue;
        }
        let Some(value) = read_attribute(b, &name, follow_symlinks)? else {
            continue;
        };
        match a_map.get(&name) {
            Some(a_value) if *a_value == value => b_count += 1,
            _ => return Ok(false),
        }
    }

    Ok(b_count == a_map.len())
}

/// Synchronises the extended attributes from `source` to `destination`.
///
/// The rsync-internal `rsync.%*` attributes (the fake-super `%stat`/`%aacl`/
/// `%dacl` channel) are excluded from both the copy and the delete pass: a
/// local single-`-X` copy is upstream's `am_sender && preserve_xattrs < 2`
/// case, which never transfers them as -X data. Excluding them from the delete
/// pass also protects the `%stat` that the fake-super metadata step writes on
/// the destination independently of the xattr transfer.
// upstream: xattrs.c:259-267 rsync_xal_get() - rsync.%FOO skipped for the sender.
pub fn sync_xattrs(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    filters: XattrSyncFilters<'_>,
    confine_root: Option<&Path>,
) -> Result<(), MetadataError> {
    // Screening first and filtering second dropped `--filter='+x trusted.foo'`
    // for a non-root receiver on Linux: the name never survived to reach the
    // rule that was meant to admit it.
    let screen = namespace_screen(filters.governs_selection(), receiver_user_only());
    let XattrSyncFilters {
        source: source_filter,
        destination: destination_filter,
    } = filters;
    let source_attrs = list_attributes(source, follow_symlinks, screen)?;
    let mut retained: HashSet<Vec<u8>> = HashSet::with_capacity(source_attrs.len());
    let dest = DestinationXattrs::open(destination, follow_symlinks, confine_root);

    for name in &source_attrs {
        let name_str = String::from_utf8_lossy(name);
        // upstream: xattrs.c:261 - rsync's own metadata channel is never copied.
        if protocol::xattr::is_rsync_internal(&name_str) {
            retained.insert(name.clone());
            continue;
        }
        // Retain ONLY what the source side actually transfers. upstream's
        // prune (xattrs.c:1074-1094) walks the destination list and removes any
        // name absent from the sender's `xalp`; a name the sender's
        // `rsync_xal_get` screened out (xattrs.c:262-264) never enters `xalp`,
        // so upstream DELETES the destination copy. Inserting before the filter
        // exempted it instead, leaving a stale value behind - measured against
        // rsync 3.5.0 with `--filter='Hx user.shared'` present on both sides:
        // upstream removes it, oc kept it.
        let allow = source_filter.is_none_or(|predicate| predicate(&name_str));

        if !allow {
            continue;
        }
        retained.insert(name.clone());

        if let Some(value) = read_attribute(source, name, follow_symlinks)? {
            dest.write(name, &value)?;
        } else {
            dest.remove(name)?;
        }
    }

    let destination_attrs = list_attributes(destination, follow_symlinks, screen)?;
    for name in &destination_attrs {
        if retained.contains(name) {
            continue;
        }

        let name_str = String::from_utf8_lossy(name);
        // Never delete rsync's own metadata channel (fake-super %stat/%aacl/%dacl):
        // it is managed separately and is not part of the -X data set.
        if protocol::xattr::is_rsync_internal(&name_str) {
            continue;
        }

        let allow = destination_filter.is_none_or(|predicate| predicate(&name_str));

        if allow {
            dest.remove(name)?;
        }
    }

    Ok(())
}

/// Applies parsed xattrs from a wire protocol [`XattrList`] to a destination file.
///
/// This is the receiver-side counterpart to [`sync_xattrs`], used when xattr
/// data arrives over the wire rather than being read from a local source file.
/// The `XattrList` entries are expected to already have local-format names
/// (translated via `wire_to_local()` during file list reception).
///
/// The function performs a full synchronization:
/// - Sets each non-abbreviated entry from the list on the destination.
/// - Resolves each abbreviated entry (checksum-only) against the `basis` file,
///   applying the basis value when its length and digest match the reference.
/// - Removes destination xattrs not present in the source list.
/// - Respects platform namespace filtering via privilege checks.
///
/// # Arguments
///
/// * `destination` - Path to apply xattrs to.
/// * `xattr_list` - Parsed xattr name-value pairs from the wire protocol.
/// * `follow_symlinks` - Whether to follow symlinks when setting xattrs.
/// * `basis` - The fnamecmp (basis) file whose existing attributes an
///   abbreviated entry references. `None` disables resolution, leaving any
///   abbreviated entry's existing destination value in place (its name is still
///   protected from the removal sweep, matching upstream's `still_abbrev`).
/// * `filter` - Optional `x`-modifier filter predicate. When present, a name for
///   which it returns `false` is neither applied to the destination nor removed
///   from it, mirroring upstream's `saw_xattr_filter` screening.
///
/// # Upstream Reference
///
/// Mirrors `xattrs.c:rsync_xal_set()` - applies received xattr data to
/// destination files. For an abbreviated entry (`XATTR_ABBREV`, a value longer
/// than `MAX_FULL_DATUM` carried only as a digest), upstream re-reads the value
/// from `fnamecmp` via `get_xattr_data()` and confirms the length and
/// `xattr_sum_nni` digest match before applying it; a mismatch takes the
/// `still_abbrev` path (xattrs.c:964-1009). The `filter` argument mirrors the
/// receive-side screening upstream performs in `receive_xattr()` (xattrs.c:822,
/// drop an excluded received name) and `rsync_xal_set()` (xattrs.c:1026, keep an
/// excluded destination name), both gated on `name_is_excluded(name,
/// NAME_IS_XATTR, ALL_FILTERS)`.
pub fn apply_xattrs_from_list(
    destination: &Path,
    xattr_list: &XattrList,
    follow_symlinks: bool,
    basis: Option<&Path>,
    filter: Option<&dyn Fn(&str) -> bool>,
    confine_root: Option<&Path>,
) -> Result<(), MetadataError> {
    let mut applied_names: HashSet<Vec<u8>> = HashSet::with_capacity(xattr_list.len());

    // Route the reserved Windows SDDL xattr to the DACL apply path on
    // Windows so the descriptor lands on the security stream rather than
    // an ADS. On other platforms the entry is dropped silently to avoid
    // surfacing meaningless `user.win32.security_descriptor` ADS-style
    // attributes on POSIX filesystems.
    #[cfg(all(feature = "acl", windows))]
    {
        let applied = crate::acl_windows::apply_sddl_from_xattrs(destination, xattr_list)?;
        if applied {
            applied_names.insert(crate::acl_windows::WINDOWS_SDDL_XATTR_NAME.to_vec());
        }
    }

    // upstream: xattrs.c:set_xattr - a non-root receiver targeting a file that
    // lacks owner write permission temporarily adds S_IWUSR so the following
    // xattr set/remove syscalls do not fail with EACCES/EPERM. The batch holds
    // that permission across both the write loop and the stale-attr removal
    // loop below, and restores the original mode on every exit path.
    let dest = DestinationXattrs::open(destination, follow_symlinks, confine_root);

    for entry in xattr_list.iter() {
        let name_str = entry.name_str();
        if !is_xattr_permitted(&name_str, receiver_user_only()) {
            continue;
        }

        // upstream: xattrs.c:822 receive_xattr() drops a received xattr whose
        // name is excluded by an `x`-modifier filter rule before it is stored,
        // so it is never applied to the destination.
        if !filter.is_none_or(|predicate| predicate(&name_str)) {
            continue;
        }

        // Skip the reserved SDDL slot on every platform: Windows has
        // already applied it via the DACL path, POSIX targets do not have
        // a corresponding native sink.
        if is_reserved_sddl_xattr(entry.name()) {
            applied_names.insert(entry.name().to_vec());
            continue;
        }

        let name_bytes = entry.name().to_vec();

        // upstream: xattrs.c:964-1009 rsync_xal_set() - an abbreviated entry
        // carries only a digest of a value the sender expects the receiver to
        // already hold on the basis (fnamecmp) file. Resolve the full value
        // locally by re-reading the basis attribute and confirming its length
        // and digest match the reference; on a match apply the resolved value.
        // The name stays in `applied_names` whether or not it resolves, so an
        // existing destination value survives the removal sweep exactly as
        // upstream's `still_abbrev` leaves `rxas[i].name` in the kept set.
        if entry.is_abbreviated() {
            applied_names.insert(name_bytes.clone());
            // upstream: get_xattr_data(fnamecmp, name, &len, 1) returning NULL -
            // a missing basis file or absent attribute - takes the still_abbrev
            // path (continue), never a fatal error, so a read failure here is
            // swallowed rather than propagated.
            if let Some(basis_path) = basis
                && let Ok(Some(value)) = read_attribute(basis_path, &name_bytes, follow_symlinks)
                && value.len() == entry.datum_len()
                // upstream: sum_end()/memcmp against rxas[i].datum+1; oc stores
                // the bare digest, so compare against `entry.datum()`. The seed
                // is ignored by the MD5 default (proto 30-32), see
                // protocol::xattr::wire::compute_xattr_checksum.
                && protocol::xattr::checksum_matches(entry.datum(), &value, 0)
            {
                // upstream: sys_lsetxattr(fname, name, ptr, len). Applying the
                // resolved basis value is idempotent when destination == basis.
                dest.write(&name_bytes, &value)?;
            }
            continue;
        }

        dest.write(&name_bytes, entry.datum())?;
        applied_names.insert(name_bytes);
    }

    // Remove destination xattrs not in the source list
    let dest_attrs = list_attributes(destination, follow_symlinks, receiver_screen())?;
    for name in &dest_attrs {
        if !applied_names.contains(name) {
            let name_str = String::from_utf8_lossy(name);
            // upstream: xattrs.c:1026 rsync_xal_set() skips the removal of a
            // destination xattr whose name is excluded by an `x`-modifier
            // filter rule, leaving the pre-existing value in place.
            if is_xattr_permitted(&name_str, receiver_user_only())
                && filter.is_none_or(|predicate| predicate(&name_str))
            {
                dest.remove(name)?;
            }
        }
    }

    Ok(())
}

/// A batch of extended-attribute mutations against one destination file.
///
/// Upstream performs every destination-side xattr mutation inside
/// `set_xattr()`, which grants a temporary `S_IWUSR` for the duration of the
/// whole batch and restores the original mode once the batch ends
/// (`xattrs.c:1090-1106`); the set loop and the removal loop beneath it in
/// `rsync_xal_set()` both run under that single grant.
///
/// oc reaches the same destination state from three entry points
/// ([`sync_xattrs`], [`apply_xattrs_from_list`] and [`strip_source_xattrs`]),
/// so the grant is expressed as a type that owns the batch rather than as a
/// line each entry point has to remember: a caller cannot mutate a destination
/// attribute without having opened the batch that holds the permission.
struct DestinationXattrs<'a> {
    path: &'a Path,
    follow_symlinks: bool,
    sink: XattrSink,
    /// Held for the batch's lifetime; restores the original mode on drop.
    #[cfg(unix)]
    _write_permission: Option<TempWritePermission>,
}

/// Where a destination batch actually writes.
///
/// `lsetxattr` declines to follow the *leaf* symlink but the kernel still
/// follows symlinks in PARENT components, and it re-walks them on every
/// call. A parent flipped to a symlink mid-batch therefore redirects an
/// attacker-chosen attribute outside the destination tree. Upstream closes
/// that by driving the writes off a held `O_NOFOLLOW` fd, and by refusing
/// outright when it cannot get one - never by falling back to the path.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/xattrs.c:386-390` - `fd >= 0 ? sys_fsetxattr : sys_lsetxattr`
/// - `rsync-3.5.0/rsync.c:519` - `xattr_refuse`, "no confined fd for a
///   slashed path: skip path-based xattr/ACL"
enum XattrSink {
    /// The path-based l-variant. Upstream's `fd < 0` arm: what a
    /// non-hardened receiver uses, and what oc uses when no confinement
    /// root is supplied.
    Path,
    /// A confined `O_NOFOLLOW` fd pinning the leaf. A parent flipped after
    /// the pin cannot redirect the write.
    #[cfg(unix)]
    Pinned(std::fs::File),
    /// The pin was required and failed on a slashed path. Every mutation is
    /// skipped: falling back to `Path` here would reinstate exactly the
    /// redirect the pin exists to refuse.
    #[cfg(unix)]
    Refused,
}

impl<'a> DestinationXattrs<'a> {
    /// Opens a mutation batch on `path`, granting the temporary owner-write
    /// permission when the file lacks it.
    ///
    /// `confine_root`, when supplied, is the destination tree the leaf must
    /// stay beneath; `path` is then resolved through the confined walk and
    /// pinned. `None` keeps upstream's path-based arm for a receiver that is
    /// not hardened.
    fn open(path: &'a Path, follow_symlinks: bool, confine_root: Option<&Path>) -> Self {
        Self {
            path,
            follow_symlinks,
            sink: Self::pin(path, confine_root),
            #[cfg(unix)]
            _write_permission: TempWritePermission::grant(path),
        }
    }

    #[cfg(unix)]
    fn pin(path: &Path, confine_root: Option<&Path>) -> XattrSink {
        let Some(root) = confine_root else {
            return XattrSink::Path;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            // Not beneath the root the caller named: the caller, not this
            // batch, is the layer that knows what that means. Keep the
            // pre-existing behaviour rather than inventing a refusal here.
            return XattrSink::Path;
        };
        // upstream: rsync.c:589-594 - the flag comes from the INTENDED type,
        // and a directory leaf needs O_DIRECTORY.
        let kind = if path.is_dir() {
            fast_io::DestLeafKind::Directory
        } else {
            fast_io::DestLeafKind::NonDirectory
        };
        match fast_io::pin_dest_leaf_confined(root, relative, kind) {
            Ok(file) => XattrSink::Pinned(file),
            // upstream: rsync.c:597 - `if (held_fd < 0 && strchr(fname, '/'))
            // xattr_refuse = 1;`. A single-component name has no parent to
            // flip, so the path-based call is still safe there.
            Err(_) if relative.parent().is_some_and(|p| !p.as_os_str().is_empty()) => {
                XattrSink::Refused
            }
            Err(_) => XattrSink::Path,
        }
    }

    #[cfg(not(unix))]
    fn pin(_path: &Path, _confine_root: Option<&Path>) -> XattrSink {
        // The confined pin is Unix-only; Windows destination attributes go
        // through the ADS/DACL layer, which does not have this shape.
        XattrSink::Path
    }

    /// Sets one attribute. upstream: `sys_fsetxattr` / `sys_lsetxattr` in
    /// `rsync_xal_set()`, chosen by whether a confined fd is held.
    fn write(&self, name: &[u8], value: &[u8]) -> Result<(), MetadataError> {
        match &self.sink {
            XattrSink::Path => write_attribute(self.path, name, value, self.follow_symlinks),
            #[cfg(unix)]
            XattrSink::Pinned(file) => crate::xattr_unix::write_attribute_at(file, name, value)
                .map_err(|error| map_xattr_error("write extended attribute", self.path, error)),
            #[cfg(unix)]
            XattrSink::Refused => Ok(()),
        }
    }

    /// Removes one attribute. upstream: `sys_lremovexattr` in
    /// `rsync_xal_set()` (`xattrs.c:1043`), with the same fd choice.
    fn remove(&self, name: &[u8]) -> Result<(), MetadataError> {
        match &self.sink {
            XattrSink::Path => remove_attribute(self.path, name, self.follow_symlinks),
            #[cfg(unix)]
            XattrSink::Pinned(file) => crate::xattr_unix::remove_attribute_at(file, name)
                .map_err(|error| map_xattr_error("remove extended attribute", self.path, error)),
            #[cfg(unix)]
            XattrSink::Refused => Ok(()),
        }
    }
}

/// RAII guard that temporarily grants owner write permission on a file so its
/// extended attributes can be modified, restoring the original mode on drop.
///
/// upstream: xattrs.c:set_xattr - `lsetxattr`/`lremovexattr` fail with
/// `EACCES`/`EPERM` when a non-root caller targets a file it owns but that
/// lacks `S_IWUSR`. Upstream adds `S_IWUSR` via `do_chmod_at` before the set
/// and restores the original mode afterwards; this guard mirrors that dance and
/// makes restoration exception-safe by tying it to `Drop`.
#[cfg(unix)]
struct TempWritePermission {
    path: std::path::PathBuf,
    /// Original permission bits (masked to `CHMOD_BITS`) to restore on drop.
    original_bits: u32,
}

#[cfg(unix)]
impl TempWritePermission {
    /// Owner write bit (`S_IWUSR`).
    const S_IWUSR: u32 = 0o200;
    /// Permission bits chmod may set: setuid/setgid/sticky plus rwx for all
    /// (upstream `rsync.h:CHMOD_BITS = S_ISUID|S_ISGID|S_ISVTX|ACCESSPERMS`).
    const CHMOD_BITS: u32 = 0o7777;

    /// Adds `S_IWUSR` when required, returning a guard only if the mode was
    /// actually changed. Returns `None` (a no-op) when running as root, when the
    /// target is a symlink (no meaningful owner-write bit; upstream guards with
    /// `!S_ISLNK`), when the file is already owner-writable, or when the
    /// permission change cannot be applied - matching upstream, which then just
    /// attempts the xattr operation directly.
    fn grant(path: &Path) -> Option<Self> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if crate::am_root() {
            return None;
        }
        let meta = std::fs::symlink_metadata(path).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        let original_bits = meta.mode() & Self::CHMOD_BITS;
        if original_bits & Self::S_IWUSR != 0 {
            return None;
        }
        let relaxed = original_bits | Self::S_IWUSR;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(relaxed)).ok()?;
        Some(Self {
            path: path.to_path_buf(),
            original_bits,
        })
    }
}

#[cfg(unix)]
impl Drop for TempWritePermission {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            &self.path,
            std::fs::Permissions::from_mode(self.original_bits),
        );
    }
}

/// Reserved xattr name carrying the Windows SDDL fidelity payload.
///
/// Defined here as a const so non-Windows builds (which do not compile
/// `acl_windows`) can still recognise and skip the slot.
const RESERVED_SDDL_XATTR: &[u8] = b"user.win32.security_descriptor";

/// Returns `true` when `name` matches the reserved SDDL xattr slot used
/// to carry full Windows security descriptors over the wire.
fn is_reserved_sddl_xattr(name: &[u8]) -> bool {
    name == RESERVED_SDDL_XATTR
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xattr_send::XattrRole;
    use protocol::xattr::XattrEntry;
    use std::fs;
    use tempfile::tempdir;

    /// Helper to check if xattrs are supported on the current filesystem.
    fn xattrs_supported(path: &Path) -> bool {
        let test_name = test_xattr_name("test_support");
        match write_attribute(path, &test_name, b"test", false) {
            Ok(()) => {
                let _ = remove_attribute(path, &test_name, false);
                true
            }
            Err(_) => false,
        }
    }

    /// Returns the local name of an `rsync.%<suffix>` internal attribute.
    ///
    /// Upstream's `RSYNC_PREFIX` is `user.rsync.` on Linux and `rsync.`
    /// elsewhere (`xattrs.c:66-69`).
    fn internal_xattr_name(suffix: &str) -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            format!("user.rsync.%{suffix}").into_bytes()
        }
        #[cfg(not(target_os = "linux"))]
        {
            format!("rsync.%{suffix}").into_bytes()
        }
    }

    /// Reports whether the collected list carries the local name `name`.
    ///
    /// A non-Linux sender emits `user.`-prefixed wire names for anything not
    /// already under `RSYNC_PREFIX` (`xattrs.c:518-530`), so the local name has
    /// to be translated before the lookup.
    fn wire_contains(list: &XattrList, name: &[u8]) -> bool {
        let expected = protocol::xattr::local_to_wire(name, true)
            .expect("test names are always representable on the wire");
        list.entries().iter().any(|entry| entry.name() == expected)
    }

    /// Returns the expected local xattr name for test entries.
    ///
    /// On Linux, names need the `user.` prefix. On other platforms (macOS,
    /// BSD, Windows ADS), names are used as-is since there is no namespace
    /// prefix requirement.
    fn test_xattr_name(base: &str) -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            format!("user.{base}").into_bytes()
        }
        #[cfg(not(target_os = "linux"))]
        {
            base.as_bytes().to_vec()
        }
    }

    #[test]
    fn list_attributes_returns_empty_for_file_without_xattrs() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attrs = list_attributes(&file, false, receiver_screen()).expect("list attrs");
        // May have system attributes, but should not error
        assert!(
            attrs
                .iter()
                .all(|a| !String::from_utf8_lossy(a).contains("user.custom"))
        );
    }

    #[test]
    fn write_and_read_attribute_roundtrip() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr_name = test_xattr_name("test_attr");
        let attr_value = b"test value 123";

        write_attribute(&file, &attr_name, attr_value, false).expect("write attr");

        let read_value = read_attribute(&file, &attr_name, false)
            .expect("read attr")
            .expect("attr should exist");

        assert_eq!(read_value, attr_value);
    }

    #[test]
    fn read_nonexistent_attribute_returns_none() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr_name = test_xattr_name("nonexistent");
        let result = read_attribute(&file, &attr_name, false).expect("read attr");
        assert!(result.is_none());
    }

    #[test]
    fn strip_source_xattrs_removes_shared_keeps_dest_only() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&source, "src").expect("write source");
        fs::write(&dest, "dst").expect("write dest");

        if !xattrs_supported(&source) || !xattrs_supported(&dest) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Model the clonefile scenario: the destination carries the source's
        // attribute (the clone copied it) plus one the filesystem applied on
        // its own (modeled by a dest-only name, e.g. com.apple.provenance).
        let shared = test_xattr_name("clone_leaked");
        let dest_only = test_xattr_name("fs_applied");
        write_attribute(&source, &shared, b"from-source", false).expect("write source attr");
        write_attribute(&dest, &shared, b"from-source", false).expect("write dest shared");
        write_attribute(&dest, &dest_only, b"fs", false).expect("write dest-only attr");

        strip_source_xattrs(&source, &dest, false, None).expect("strip");

        let dest_attrs = list_attributes(&dest, false, receiver_screen()).expect("list dest");
        assert!(
            !dest_attrs.contains(&shared),
            "source-originated attribute must be stripped from the destination"
        );
        assert!(
            dest_attrs.contains(&dest_only),
            "filesystem-applied (dest-only) attribute must be preserved"
        );
        // The source is read-only input to the strip and must be untouched.
        let source_attrs = list_attributes(&source, false, receiver_screen()).expect("list source");
        assert!(source_attrs.contains(&shared), "source must be untouched");
    }

    #[test]
    fn strip_source_xattrs_with_no_source_attrs_is_noop() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&source, "src").expect("write source");
        fs::write(&dest, "dst").expect("write dest");

        if !xattrs_supported(&dest) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let dest_only = test_xattr_name("keep_me");
        write_attribute(&dest, &dest_only, b"v", false).expect("write dest attr");

        strip_source_xattrs(&source, &dest, false, None).expect("strip with empty source");

        let dest_attrs = list_attributes(&dest, false, receiver_screen()).expect("list dest");
        assert!(
            dest_attrs.contains(&dest_only),
            "with no source attributes the destination is left untouched"
        );
    }

    #[test]
    fn remove_attribute_deletes_xattr() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr_name = test_xattr_name("to_remove");
        write_attribute(&file, &attr_name, b"value", false).expect("write attr");

        // Verify it exists
        assert!(
            read_attribute(&file, &attr_name, false)
                .expect("read")
                .is_some()
        );

        remove_attribute(&file, &attr_name, false).expect("remove attr");

        // Verify it's gone
        assert!(
            read_attribute(&file, &attr_name, false)
                .expect("read after remove")
                .is_none()
        );
    }

    #[test]
    fn sync_xattrs_copies_attributes() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr1 = test_xattr_name("attr1");
        let attr2 = test_xattr_name("attr2");
        write_attribute(&source, &attr1, b"value1", false).expect("write attr1");
        write_attribute(&source, &attr2, b"value2", false).expect("write attr2");

        sync_xattrs(
            &source,
            &destination,
            false,
            XattrSyncFilters::default(),
            None,
        )
        .expect("sync");

        assert_eq!(
            read_attribute(&destination, &attr1, false)
                .expect("read")
                .expect("attr1"),
            b"value1"
        );
        assert_eq!(
            read_attribute(&destination, &attr2, false)
                .expect("read")
                .expect("attr2"),
            b"value2"
        );
    }

    // upstream: xattrs.c xattrs_differ() - a changed value or a missing/extra
    // transferable attr means the pair differs; the rsync.%* channel is ignored.
    #[test]
    fn xattrs_match_detects_value_and_membership_differences() {
        let dir = tempdir().expect("create temp dir");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "a").expect("write a");
        fs::write(&b, "b").expect("write b");

        if !xattrs_supported(&a) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let name = test_xattr_name("nice");
        write_attribute(&a, &name, b"this is nice, but different", false).expect("write a nice");
        write_attribute(&b, &name, b"this is nice", false).expect("write b nice");
        assert!(!xattrs_match(&a, &b, false).expect("compare differing values"));

        // Equal values -> match.
        write_attribute(&b, &name, b"this is nice, but different", false).expect("update b nice");
        assert!(xattrs_match(&a, &b, false).expect("compare equal values"));

        // An extra rsync-internal attr on either side is ignored.
        let stat_name: Vec<u8> = if cfg!(target_os = "linux") {
            b"user.rsync.%stat".to_vec()
        } else {
            b"rsync.%stat".to_vec()
        };
        if write_attribute(&b, &stat_name, b"100644 0,0 1:1", false).is_ok() {
            assert!(
                xattrs_match(&a, &b, false).expect("compare ignoring %stat"),
                "rsync.%stat must not affect the xattr match"
            );
        }
    }

    #[test]
    fn sync_xattrs_removes_extra_dest_attributes() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Source has attr1, destination has attr1 and attr2
        let attr1 = test_xattr_name("attr1");
        let attr2 = test_xattr_name("extra");
        write_attribute(&source, &attr1, b"value1", false).expect("write source attr1");
        write_attribute(&destination, &attr1, b"old_value1", false).expect("write dest attr1");
        write_attribute(&destination, &attr2, b"extra_value", false).expect("write dest attr2");

        sync_xattrs(
            &source,
            &destination,
            false,
            XattrSyncFilters::default(),
            None,
        )
        .expect("sync");

        // attr1 should be updated
        assert_eq!(
            read_attribute(&destination, &attr1, false)
                .expect("read")
                .expect("attr1"),
            b"value1"
        );
        // attr2 should be removed (not in source)
        assert!(
            read_attribute(&destination, &attr2, false)
                .expect("read")
                .is_none()
        );
    }

    // upstream: xattrs.c:259-267 - the rsync.%stat fake-super channel is never
    // part of the -X data set. It must not be copied from the source, and a
    // fake-super-written %stat already on the destination must survive the
    // delete pass. Regression guard for the `xattrs` conformance test's
    // `--fake-super --chmod=a=` leg.
    #[test]
    fn sync_xattrs_leaves_rsync_internal_stat_untouched() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let stat_name: Vec<u8> = if cfg!(target_os = "linux") {
            b"user.rsync.%stat".to_vec()
        } else {
            b"rsync.%stat".to_vec()
        };

        // Source carries a stale %stat; destination carries the fake-super one.
        if write_attribute(&source, &stat_name, b"100644 0,0 1:1", false).is_err() {
            eprintln!("rsync.%stat namespace not writable, skipping");
            return;
        }
        write_attribute(&destination, &stat_name, b"100000 0,0 2:2", false)
            .expect("write dest %stat");

        sync_xattrs(
            &source,
            &destination,
            false,
            XattrSyncFilters::default(),
            None,
        )
        .expect("sync");

        // The destination's fake-super %stat must be preserved verbatim - not
        // overwritten by the source copy nor deleted by the removal pass.
        assert_eq!(
            read_attribute(&destination, &stat_name, false)
                .expect("read")
                .expect("dest %stat survives"),
            b"100000 0,0 2:2",
        );
    }

    /// `-X` strips the fake-super store from the wire but `-XX` transmits it.
    ///
    /// This is the whole point of the doubled option: `--fake-super` writes a
    /// file's real ownership and permissions into `rsync.%stat`, and only a
    /// second `-X` moves that store to the peer so an unprivileged mirror can
    /// be re-materialised later. Collapsing the level to a bool makes `-XX` a
    /// silent synonym for `-X` while the help text still advertises the store.
    ///
    /// upstream: xattrs.c:261-263 - `if (name_len > RPRE_LEN && name[RPRE_LEN]
    /// == '%' && HAS_PREFIX(name, RSYNC_PREFIX)) { if ((am_sender &&
    /// preserve_xattrs < 2) ...) continue; }`
    #[test]
    fn sender_transmits_the_fake_super_store_only_at_level_two() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("stored.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let stat_name = internal_xattr_name("stat");
        // A plausible fake-super payload: `MODE UID,GID MAJOR:MINOR`, the
        // format upstream's set_stat_xattr() writes.
        if write_attribute(&file, &stat_name, b"100644 0,0 0:0", false).is_err() {
            eprintln!("filesystem rejects rsync.% names, skipping test");
            return;
        }
        let plain = test_xattr_name("keepme");
        write_attribute(&file, &plain, b"v", false).expect("write plain attr");

        let single = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                preserve_xattrs: 1,
                ..XattrSendOptions::new(XattrRole::Sender)
            },
        )
        .expect("read at level 1");
        assert!(
            !wire_contains(&single, &stat_name),
            "a single -X must strip the fake-super store",
        );
        assert!(
            wire_contains(&single, &plain),
            "a single -X must still carry ordinary attributes",
        );

        let double = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                preserve_xattrs: 2,
                ..XattrSendOptions::new(XattrRole::Sender)
            },
        )
        .expect("read at level 2");
        assert!(
            wire_contains(&double, &stat_name),
            "-XX must transmit the fake-super store, otherwise it is a no-op alias for -X",
        );
    }

    /// `--fake-super` suppresses the three store attributes at any level.
    ///
    /// With `--fake-super` active the local `rsync.%stat` / `%aacl` / `%dacl`
    /// describe *this* side's emulated ownership, not the file's own metadata,
    /// so copying them onward would hand the peer a stale store. Other
    /// `rsync.%` names are unaffected and remain governed by the level alone.
    ///
    /// upstream: xattrs.c:263-266 - `am_root < 0 && (strcmp(..., XSTAT_SUFFIX)
    /// == 0 || ... XACC_ACL_SUFFIX ... || ... XDEF_ACL_SUFFIX ...)`, where
    /// `am_root < 0` is what `set_fake_super()` sets.
    #[test]
    fn fake_super_drops_the_store_attrs_even_at_level_two() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("faked.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let stat_name = internal_xattr_name("stat");
        if write_attribute(&file, &stat_name, b"100644 0,0 0:0", false).is_err() {
            eprintln!("filesystem rejects rsync.% names, skipping test");
            return;
        }
        let aacl_name = internal_xattr_name("aacl");
        write_attribute(&file, &aacl_name, b"acl", false).expect("write %aacl");
        let dacl_name = internal_xattr_name("dacl");
        write_attribute(&file, &dacl_name, b"acl", false).expect("write %dacl");
        // Not one of the three store suffixes, so --fake-super leaves it alone.
        let other_name = internal_xattr_name("other");
        write_attribute(&file, &other_name, b"o", false).expect("write %other");

        let list = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                preserve_xattrs: 2,
                fake_super: true,
                ..XattrSendOptions::new(XattrRole::Sender)
            },
        )
        .expect("read under fake-super");

        for name in [&stat_name, &aacl_name, &dacl_name] {
            assert!(
                !wire_contains(&list, name),
                "--fake-super must suppress {} even at -XX",
                String::from_utf8_lossy(name),
            );
        }
        assert!(
            wire_contains(&list, &other_name),
            "--fake-super gates only %stat/%aacl/%dacl, not every rsync.% name",
        );
    }

    /// The generator keeps `rsync.%FOO` at a single `-X`, the sender does not.
    ///
    /// Upstream's strip is `am_sender && preserve_xattrs < 2`, so the role is
    /// load-bearing: the generator reads the destination to diff it, and a
    /// destination that already carries the store must compare equal to a
    /// sender list that carries it too. Dropping it on the generator side
    /// would make every `-XX` re-run report a phantom `x` change.
    ///
    /// upstream: xattrs.c:262 - `if ((am_sender && preserve_xattrs < 2) ...)`
    #[test]
    fn generator_keeps_the_rsync_store_at_a_single_x() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let stat_name = internal_xattr_name("stat");
        if write_attribute(&file, &stat_name, b"100644 0,0 0:0", false).is_err() {
            eprintln!("filesystem rejects rsync.% names, skipping test");
            return;
        }

        let generator = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                preserve_xattrs: 1,
                ..XattrSendOptions::new(XattrRole::Generator)
            },
        )
        .expect("read as the generator");
        assert!(
            wire_contains(&generator, &stat_name),
            "the rsync.% strip is gated on am_sender, so the generator keeps it",
        );

        let sender = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                preserve_xattrs: 1,
                ..XattrSendOptions::new(XattrRole::Sender)
            },
        )
        .expect("read as the sender");
        assert!(
            !wire_contains(&sender, &stat_name),
            "the sender must still strip the store below -XX",
        );
    }

    /// A non-root generator screens away every namespace it could not store.
    ///
    /// This is what keeps the itemize `x` column honest: upstream's non-root
    /// receiver can only write `user.*` (`xattrs.c:830`), so comparing a
    /// `security.*` or `trusted.*` name it will never touch would report a
    /// change that upstream does not report and that no transfer could ever
    /// settle. The sender is exempt at any privilege level, which is why the
    /// role - not `am_root` alone - decides.
    ///
    /// upstream: xattrs.c:249 - `int user_only = am_sender ? 0 : !am_root;`
    #[cfg(target_os = "linux")]
    #[test]
    fn non_root_generator_screens_out_non_user_namespaces() {
        let cases = [
            "user.mine",
            "security.selinux",
            "trusted.overlay.opaque",
            "system.posix_acl_access",
        ];

        let permitted = |role: XattrRole, am_root: bool| {
            cases
                .into_iter()
                .filter(|name| is_xattr_permitted(name, role.user_only(am_root)))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            permitted(XattrRole::Generator, false),
            vec!["user.mine"],
            "a non-root generator diffs the user namespace alone",
        );
        assert_eq!(
            permitted(XattrRole::Generator, true),
            vec!["user.mine", "security.selinux", "trusted.overlay.opaque"],
            "a root generator regains everything but system.*",
        );
        assert_eq!(
            permitted(XattrRole::Sender, false),
            vec!["user.mine", "security.selinux", "trusted.overlay.opaque"],
            "am_sender pins user_only to 0, so an unprivileged sender is unchanged",
        );
    }

    /// An `x`-modifier filter screens the names the sender collects.
    ///
    /// upstream: xattrs.c:250-252 - `if (saw_xattr_filter) { if
    /// (name_is_excluded(name, NAME_IS_XATTR, ALL_FILTERS)) continue; }`
    #[test]
    fn sender_applies_the_xattr_name_filter() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("filtered.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let dropped = test_xattr_name("foo");
        let kept = test_xattr_name("bar");
        write_attribute(&file, &dropped, b"d", false).expect("write foo");
        write_attribute(&file, &kept, b"k", false).expect("write bar");

        let dropped_str = String::from_utf8(dropped.clone()).expect("utf8 name");
        let predicate = |name: &str| name != dropped_str;
        let list = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                filter: Some(&predicate),
                ..XattrSendOptions::new(XattrRole::Sender)
            },
        )
        .expect("read with filter");

        assert!(
            !wire_contains(&list, &dropped),
            "an excluded name must never be collected",
        );
        assert!(
            wire_contains(&list, &kept),
            "a name the filter admits must still be collected",
        );
    }

    /// A filter replaces the namespace rule rather than stacking on top of it.
    ///
    /// Upstream's two branches are mutually exclusive: `if (saw_xattr_filter)
    /// {...} else if (user_only ? ... : HAS_PREFIX(name, SYSTEM_PREFIX))
    /// continue;` (xattrs.c:250-257). So with any `x`-modifier rule present a
    /// `system.*` name that the namespace rule would have dropped is instead
    /// judged by the filter alone. Screening on both would silently ignore an
    /// admitting rule the user wrote.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_filter_bypasses_the_namespace_check() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("ns.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // system.* is writable only with privileges (and only on filesystems
        // that back it), so treat an unwritable name as "cannot exercise".
        let system_name = b"system.test_ns".to_vec();
        if write_attribute(&file, &system_name, b"s", false).is_err() {
            eprintln!("cannot set a system.* xattr here, skipping test");
            return;
        }

        // Without a filter the namespace rule drops it (xattrs.c:256).
        let unfiltered = read_xattrs_for_wire(&file, &XattrSendOptions::new(XattrRole::Sender))
            .expect("read without filter");
        assert!(
            !wire_contains(&unfiltered, &system_name),
            "the namespace rule drops system.* for a non-root sender",
        );

        // With a filter that admits everything, the namespace rule is not
        // evaluated at all and the name survives.
        let admit_all = |_name: &str| true;
        let filtered = read_xattrs_for_wire(
            &file,
            &XattrSendOptions {
                filter: Some(&admit_all),
                ..XattrSendOptions::new(XattrRole::Sender)
            },
        )
        .expect("read with filter");
        assert!(
            wire_contains(&filtered, &system_name),
            "an x-modifier filter replaces the namespace rule, it does not stack with it",
        );
    }

    // upstream: xattrs.c:297-299 assigns each xattr num = sorted_position + 1
    // (ascending). The receiver re-derives the identical 1-based ascending num
    // in receive order (protocol::xattr::cache `for num in 1..=count`). The
    // abbreviated (>MAX_FULL_DATUM) value request round-trip keys on num, so the
    // sender's assignment MUST be ascending: a descending assignment makes the
    // sender resolve a request for the first entry (num 1) to the last entry and
    // return a different xattr's value. The num is assigned identically for every
    // entry regardless of value size, so small values suffice to pin the order
    // (large-value round-trip fidelity varies across xattr backends).
    #[test]
    fn read_xattrs_for_wire_num_is_ascending_like_the_receiver() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("multi.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Three attrs with distinct names so the sorted order is unambiguous.
        write_attribute(&file, &test_xattr_name("aaa"), b"a", false).expect("write aaa");
        write_attribute(&file, &test_xattr_name("mmm"), b"m", false).expect("write mmm");
        write_attribute(&file, &test_xattr_name("zzz"), b"z", false).expect("write zzz");

        let list = read_xattrs_for_wire(&file, &XattrSendOptions::new(XattrRole::Sender))
            .expect("read xattrs");
        let entries = list.entries();
        assert!(entries.len() >= 3, "expected at least our three xattrs");

        // num is 1-based ascending over the name-sorted order - identical to the
        // receiver's `1..=count`, so a num request resolves to the same entry.
        // The old descending assignment (count - i) gives entries[0].num() ==
        // count != 1 and fails here.
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(
                entry.num(),
                (i + 1) as u32,
                "entry {i} ({}) must carry ascending 1-based num to match the receiver",
                entry.name_str(),
            );
            if i > 0 {
                assert!(
                    entries[i - 1].name() <= entry.name(),
                    "entries must be sorted ascending by wire name",
                );
            }
        }
    }

    #[test]
    fn sync_xattrs_with_filter_skips_filtered_attrs() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let allowed = test_xattr_name("allowed");
        let blocked = test_xattr_name("blocked");
        write_attribute(&source, &allowed, b"allowed_val", false).expect("write allowed");
        write_attribute(&source, &blocked, b"blocked_val", false).expect("write blocked");

        // Filter that only allows attrs NOT containing "blocked"
        let filter = |name: &str| !name.contains("blocked");
        sync_xattrs(
            &source,
            &destination,
            false,
            XattrSyncFilters::uniform(&filter),
            None,
        )
        .expect("sync");

        // allowed should be synced
        assert_eq!(
            read_attribute(&destination, &allowed, false)
                .expect("read")
                .expect("allowed"),
            b"allowed_val"
        );
        // blocked should NOT be synced
        assert!(
            read_attribute(&destination, &blocked, false)
                .expect("read")
                .is_none()
        );
    }

    /// An `x`-modifier filter REPLACES the namespace screen, it does not stack
    /// with it: upstream reaches the namespace test only through the `else` of
    /// the `saw_xattr_filter` branch (xattrs.c:250-257, :375-382). Screening
    /// first and filtering second made `--filter='+x trusted.foo'` inert for a
    /// non-root Linux receiver - the name was gone before its rule ran.
    ///
    /// The decision is pinned here rather than through a real xattr because no
    /// unprivileged fixture can express it: writing `trusted.*` needs
    /// CAP_SYS_ADMIN on Linux, and off Linux `is_xattr_permitted` is constantly
    /// true, so `Apply` and `Bypass` are indistinguishable by observation.
    #[test]
    fn a_filter_replaces_the_namespace_screen_rather_than_stacking() {
        assert!(matches!(
            namespace_screen(true, true),
            NamespaceScreen::Bypass
        ));
        assert!(matches!(
            namespace_screen(true, false),
            NamespaceScreen::Bypass
        ));
    }

    /// Non-vacuity companion: without a filter the screen still applies, and
    /// carries `user_only` through unchanged. Without this row the rule above
    /// would also hold for a `namespace_screen` that returned `Bypass` always.
    #[test]
    fn without_a_filter_the_namespace_screen_applies_verbatim() {
        assert!(matches!(
            namespace_screen(false, true),
            NamespaceScreen::Apply { user_only: true }
        ));
        assert!(matches!(
            namespace_screen(false, false),
            NamespaceScreen::Apply { user_only: false }
        ));
    }

    #[test]
    fn is_xattr_permitted_allows_user_namespace() {
        // user.* should always be permitted regardless of platform or mode.
        assert!(is_xattr_permitted("user.test", false));
        assert!(is_xattr_permitted("user.rsync.%stat", false));
        assert!(is_xattr_permitted("user.test", true));
    }

    /// A non-root sender (upstream `user_only == false`) must transmit every
    /// namespace except `system.*`. This regresses a fidelity gap where oc
    /// restricted a non-root sender to the `user.*` namespace and silently
    /// dropped `security.*` xattrs (e.g. SELinux labels) and `trusted.*`, which
    /// upstream `rsync_xal_get()` sends because `user_only = am_sender ? 0 :
    /// !am_root` is always 0 on the sender. The decision is exercised directly
    /// with representative names so it runs deterministically as an ordinary
    /// user - setting real `security.*` xattrs would require privilege.
    #[test]
    #[cfg(target_os = "linux")]
    fn is_xattr_permitted_sender_skips_only_system_namespace() {
        // upstream: xattrs.c:256 rsync_xal_get() with user_only == false takes
        // the `HAS_PREFIX(name, SYSTEM_PREFIX)` branch: skip only system.*.
        assert!(is_xattr_permitted("user.foo", false));
        assert!(
            is_xattr_permitted("security.selinux", false),
            "a non-root sender must transmit security.* xattrs (SELinux labels)"
        );
        assert!(is_xattr_permitted("trusted.test", false));
        assert!(
            !is_xattr_permitted("system.posix_acl_access", false),
            "the system.* namespace is never sent"
        );
    }

    /// A non-root receiver (upstream `user_only == true`) keeps only the
    /// `user.*` namespace, matching upstream's non-root store restriction at
    /// `xattrs.c:830`.
    #[test]
    #[cfg(target_os = "linux")]
    fn is_xattr_permitted_receiver_user_only_keeps_only_user_namespace() {
        // upstream: xattrs.c:256 rsync_xal_get() with user_only == true takes
        // the `!HAS_PREFIX(name, USER_PREFIX)` branch: keep only user.*.
        assert!(is_xattr_permitted("user.foo", true));
        assert!(!is_xattr_permitted("security.selinux", true));
        assert!(!is_xattr_permitted("trusted.test", true));
        assert!(!is_xattr_permitted("system.posix_acl_access", true));
    }

    /// The receiver screen must answer upstream's `am_root`, not a raw euid.
    ///
    /// upstream: `xattrs.c:1002` `int user_only = am_root <= 0;` where
    /// `am_root` is `main.c:1846`'s cached `MY_UID() == ROOT_UID`, and
    /// `MY_UID()` is the **libc** `geteuid()` (`rsync.h:1455`). `fakeroot`
    /// interposes that libc symbol, so upstream under `fakeroot` sees
    /// `am_root == 1` and keeps every namespace but `system.*`. A raw
    /// `geteuid` syscall is invisible to the interposition, still reports the
    /// real unprivileged uid, and would drop `security.*`/`trusted.*` on the
    /// receiver that upstream stores.
    #[test]
    #[cfg(target_os = "linux")]
    fn receiver_user_only_answers_the_libc_am_root() {
        assert_eq!(
            receiver_user_only(),
            !crate::am_root(),
            "the receiver xattr screen must be the negation of the crate's \
             canonical am_root, which reads the libc geteuid"
        );
    }

    /// Under `fakeroot` the receiver screen must open up, as upstream's does.
    ///
    /// The two euid sources only disagree under an LD_PRELOAD identity fake,
    /// so this re-execs the unit-test binary under `fakeroot(1)` and asserts
    /// there. Two environments cannot host the check - no `fakeroot` on
    /// `PATH`, and a statically linked build where LD_PRELOAD is inert - and
    /// each says so rather than passing quietly.
    #[test]
    #[cfg(target_os = "linux")]
    fn receiver_user_only_is_false_under_fakeroot() {
        const MARKER: &str = "OC_RSYNC_XATTR_FAKEROOT_CHILD";
        const TEST_PATH: &str = "xattr::tests::receiver_user_only_is_false_under_fakeroot";

        if std::env::var_os(MARKER).is_some() {
            // fakeroot fakes the identity by interposing the libc symbol with
            // LD_PRELOAD, which a statically linked binary (the musl CI cell)
            // ignores entirely - measured: a `-static` probe reports the real
            // uid from both libc and the raw syscall under fakeroot, while the
            // dynamic build of the same source reports 0 from libc. Where the
            // interposition did not take, the two euid sources cannot be made
            // to disagree and there is nothing here to observe.
            if !crate::am_root() {
                eprintln!(
                    "SKIPPED {TEST_PATH}: fakeroot(1) ran but could not \
                     interpose the libc geteuid, so this binary is statically \
                     linked and LD_PRELOAD is inert"
                );
                return;
            }
            assert!(
                !receiver_user_only(),
                "upstream keeps security.*/trusted.* on a fakeroot receiver \
                 because am_root is 1 there (xattrs.c:1002)"
            );
            return;
        }

        let Some(fakeroot) = find_in_path("fakeroot") else {
            eprintln!(
                "SKIPPED {TEST_PATH}: fakeroot(1) is not on PATH, so the libc \
                 and raw-syscall effective uids cannot be made to disagree"
            );
            return;
        };

        let exe = std::env::current_exe().expect("locate the unit-test binary");
        let output = std::process::Command::new(fakeroot)
            .arg(&exe)
            .args(["--exact", TEST_PATH, "--nocapture", "--test-threads=1"])
            .env(MARKER, "1")
            .output()
            .expect("re-exec the unit-test binary under fakeroot");

        assert!(
            output.status.success(),
            "the fakeroot leg failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        // Re-emit a skip the child decided on, so a leg that could not observe
        // anything says so here too instead of reading as a clean pass.
        for line in String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("SKIPPED"))
        {
            eprintln!("{line}");
        }
    }

    /// Resolves a bare program name against `PATH`, returning the first
    /// existing candidate. Used to decide whether the `fakeroot` leg can run.
    #[cfg(target_os = "linux")]
    fn find_in_path(program: &str) -> Option<std::path::PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn is_xattr_permitted_allows_all_on_non_linux() {
        for user_only in [false, true] {
            assert!(is_xattr_permitted("user.test", user_only));
            assert!(is_xattr_permitted("com.apple.quarantine", user_only));
            assert!(is_xattr_permitted("security.selinux", user_only));
            assert!(is_xattr_permitted("system.posix_acl_access", user_only));
        }
    }

    #[test]
    fn sync_xattrs_filter_preserves_unfiltered_dest_attrs() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let src_attr = test_xattr_name("from_source");
        let preserved = test_xattr_name("preserved");
        write_attribute(&source, &src_attr, b"source_val", false).expect("write source attr");
        write_attribute(&destination, &preserved, b"keep_me", false).expect("write preserved");

        // Filter that blocks "preserved" - it should NOT be touched
        let filter = |name: &str| !name.contains("preserved");
        sync_xattrs(
            &source,
            &destination,
            false,
            XattrSyncFilters::uniform(&filter),
            None,
        )
        .expect("sync");

        // src_attr should be synced
        assert_eq!(
            read_attribute(&destination, &src_attr, false)
                .expect("read")
                .expect("src_attr"),
            b"source_val"
        );
        // preserved should still exist (not deleted because filter blocks it)
        assert_eq!(
            read_attribute(&destination, &preserved, false)
                .expect("read")
                .expect("preserved"),
            b"keep_me"
        );
    }

    /// The source read and the destination prune consult DIFFERENT filters.
    ///
    /// upstream: the source read is the sender's `rsync_xal_get()`
    /// (xattrs.c:262) and the prune is the generator's sweep in
    /// `rsync_xal_set()` (xattrs.c:1078); `rule_matches()` elides a
    /// side-flagged rule before its pattern is consulted (exclude.c:1010), so
    /// an `H`/`S` rule reaches only the first and a `P`/`R` rule only the
    /// second.
    ///
    /// Each half is asserted in BOTH directions. With a single shared
    /// predicate the two rows of each pair collapse onto the same answer, so
    /// the inverse row is what makes this non-vacuous rather than decoration.
    #[test]
    fn sync_xattrs_consults_each_side_filter_for_its_own_half() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let from_source = test_xattr_name("from_source");
        let dest_only = test_xattr_name("dest_only");
        let name_of = |raw: &[u8]| String::from_utf8_lossy(raw).into_owned();
        let blocked = |target: String| move |name: &str| name != target;
        let allow_all = |_: &str| true;

        // Source-read half: only `filters.source` may suppress the copy.
        for (block_source, expect_copied) in [(true, false), (false, true)] {
            let _ = fs::remove_file(&destination);
            fs::write(&destination, "dest").expect("write dest");
            write_attribute(&source, &from_source, b"v", false).expect("write source attr");

            let deny = blocked(name_of(&from_source));
            let filters = if block_source {
                XattrSyncFilters {
                    source: Some(&deny),
                    destination: Some(&allow_all),
                }
            } else {
                XattrSyncFilters {
                    source: Some(&allow_all),
                    destination: Some(&deny),
                }
            };
            sync_xattrs(&source, &destination, false, filters, None).expect("sync");

            let copied = read_attribute(&destination, &from_source, false)
                .expect("read")
                .is_some();
            assert_eq!(
                copied, expect_copied,
                "a source name is gated by `filters.source` alone \
                 (block_source = {block_source})"
            );
        }

        // The row that discriminates a REAL fork from a half fork: a name
        // present on BOTH sides. `retained` must be keyed on what the source
        // side actually transfers, so a source-denied name still reaches the
        // prune loop and is deleted from the destination - upstream's
        // xattrs.c:1074-1094 removes any destination name absent from the
        // sender's `xalp`. With `retained.insert` above the source filter this
        // row is the only one that fails; the two disjoint fixtures below
        // cannot see it, which is why they are not sufficient on their own.
        // Measured against rsync 3.5.0 with `--filter='Hx user.shared'`.
        {
            let _ = fs::remove_file(&destination);
            fs::write(&destination, "dest").expect("write dest");
            write_attribute(&source, &from_source, b"src", false).expect("source attr");
            write_attribute(&destination, &from_source, b"dst", false).expect("dest attr");

            let deny = blocked(name_of(&from_source));
            sync_xattrs(
                &source,
                &destination,
                false,
                XattrSyncFilters {
                    source: Some(&deny),
                    destination: Some(&allow_all),
                },
                None,
            )
            .expect("sync");

            assert!(
                read_attribute(&destination, &from_source, false)
                    .expect("read")
                    .is_none(),
                "a source-denied name is not transferred, so the destination copy                  must be pruned - not exempted by having existed on the source"
            );
        }

        // Destination-prune half: only `filters.destination` may spare it.
        let _ = fs::remove_file(&source);
        fs::write(&source, "source").expect("rewrite source");
        for (block_destination, expect_survives) in [(true, true), (false, false)] {
            let _ = fs::remove_file(&destination);
            fs::write(&destination, "dest").expect("write dest");
            write_attribute(&destination, &dest_only, b"v", false).expect("write dest attr");

            let deny = blocked(name_of(&dest_only));
            let filters = if block_destination {
                XattrSyncFilters {
                    source: Some(&allow_all),
                    destination: Some(&deny),
                }
            } else {
                XattrSyncFilters {
                    source: Some(&deny),
                    destination: Some(&allow_all),
                }
            };
            sync_xattrs(&source, &destination, false, filters, None).expect("sync");

            let survives = read_attribute(&destination, &dest_only, false)
                .expect("read")
                .is_some();
            assert_eq!(
                survives, expect_survives,
                "an extraneous destination name is gated by `filters.destination` \
                 alone (block_destination = {block_destination})"
            );
        }
    }

    #[test]
    fn apply_xattrs_from_list_sets_attributes() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            test_xattr_name("attr1"),
            b"value1".to_vec(),
        ));
        list.push(XattrEntry::new(
            test_xattr_name("attr2"),
            b"value2".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false, None, None, None).expect("apply xattrs");

        let attr1 = test_xattr_name("attr1");
        let attr2 = test_xattr_name("attr2");

        assert_eq!(
            read_attribute(&file, &attr1, false)
                .expect("read")
                .expect("attr1"),
            b"value1"
        );
        assert_eq!(
            read_attribute(&file, &attr2, false)
                .expect("read")
                .expect("attr2"),
            b"value2"
        );
    }

    // A read-only file (mode 0o444) must still receive its xattrs: setting a
    // user xattr on a file lacking owner write permission fails EACCES/EPERM for
    // a non-root caller, so the apply path temporarily grants S_IWUSR. WHY it
    // matters: callers must not be forced to pre-relax permissions, and the file
    // must not be left writable afterwards - the original mode is restored.
    // upstream: xattrs.c:set_xattr.
    #[cfg(unix)]
    #[test]
    fn apply_xattrs_from_list_grants_and_restores_write_perm_on_readonly_file() {
        use std::os::unix::fs::PermissionsExt;

        // Running as root bypasses the permission check entirely (root can set
        // xattrs on a read-only file), so this test cannot exercise the guard.
        if crate::am_root() {
            eprintln!("running as root, skipping read-only xattr test");
            return;
        }

        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("readonly.txt");
        fs::write(&file, "content").expect("write file");

        // Probe support while the file is still writable.
        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).expect("chmod 0o444");

        let mut list = XattrList::new();
        list.push(XattrEntry::new(test_xattr_name("ro"), b"value".to_vec()));

        apply_xattrs_from_list(&file, &list, false, None, None, None)
            .expect("apply xattrs on read-only file");

        let name = test_xattr_name("ro");
        assert_eq!(
            read_attribute(&file, &name, false)
                .expect("read")
                .expect("xattr present"),
            b"value"
        );

        let restored = fs::symlink_metadata(&file)
            .expect("stat")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            restored, 0o444,
            "original read-only mode must be restored after applying xattrs"
        );
    }

    // CLASS invariant, not a single-site regression: upstream mutates a
    // destination's xattrs from exactly one place (`set_xattr`), and that place
    // holds the temporary S_IWUSR for the whole batch. oc reaches the same
    // destination state from three entry points, so the property "a read-only
    // destination is still mutable, and its mode survives" has to hold for
    // every one of them - covering only the entry that happened to be fixed
    // first is what let `sync_xattrs` and `strip_source_xattrs` ship without
    // the grant. A fourth entry point added without the batch fails here.
    // upstream: xattrs.c:1090-1106 set_xattr.
    #[cfg(unix)]
    #[test]
    fn every_destination_entry_point_mutates_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses the permission check, so the grant cannot be exercised.
        if crate::am_root() {
            eprintln!("running as root, skipping read-only xattr test");
            return;
        }

        let dir = tempdir().expect("create temp dir");
        let name = test_xattr_name("class");

        // Probe support once, on a throwaway file, so an unsupported
        // filesystem is a single visible skip rather than three legs that
        // quietly do nothing and let the test pass without asserting.
        let probe = dir.path().join("probe");
        fs::write(&probe, "content").expect("write probe");
        if !xattrs_supported(&probe) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Each leg gets a fresh source/destination pair carrying `name`, left
        // at mode 0o444 so the mutation must acquire the write permission.
        let readonly_pair = |leg: &str| {
            let source = dir.path().join(format!("{leg}.src"));
            let destination = dir.path().join(format!("{leg}.dst"));
            fs::write(&source, "content").expect("write source");
            fs::write(&destination, "content").expect("write destination");
            write_attribute(&source, &name, b"value", false).expect("seed source");
            write_attribute(&destination, &name, b"value", false).expect("seed destination");
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o444))
                .expect("chmod 0o444");
            (source, destination)
        };

        let assert_mode_restored = |destination: &Path, leg: &str| {
            let mode = fs::symlink_metadata(destination)
                .expect("stat")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o444, "{leg} must restore the original mode");
        };

        // sync_xattrs: copies the source set, then removes destination strays.
        let (source, destination) = readonly_pair("sync");
        sync_xattrs(
            &source,
            &destination,
            false,
            XattrSyncFilters::default(),
            None,
        )
        .expect("sync_xattrs on read-only dest");
        assert_mode_restored(&destination, "sync_xattrs");

        // strip_source_xattrs: removes only names the source also carries. This
        // is the macOS clonefile correction, reachable under plain -a with no
        // -X, which is how the defect surfaced.
        let (source, destination) = readonly_pair("strip");
        strip_source_xattrs(&source, &destination, false, None)
            .expect("strip_source_xattrs on read-only dest");
        assert!(
            read_attribute(&destination, &name, false)
                .expect("read")
                .is_none(),
            "strip_source_xattrs must remove the source-originated attribute"
        );
        assert_mode_restored(&destination, "strip_source_xattrs");

        // apply_xattrs_from_list: the wire-side receiver entry.
        let (_source, destination) = readonly_pair("apply");
        let mut list = XattrList::new();
        list.push(XattrEntry::new(name.clone(), b"applied".to_vec()));
        apply_xattrs_from_list(&destination, &list, false, None, None, None)
            .expect("apply_xattrs_from_list on read-only dest");
        assert_eq!(
            read_attribute(&destination, &name, false)
                .expect("read")
                .expect("attribute present"),
            b"applied"
        );
        assert_mode_restored(&destination, "apply_xattrs_from_list");
    }

    // Receive-side mirror of the send-side x-modifier filter (#128): a received
    // xattr whose name the filter excludes must never be applied to the
    // destination, even though the sender put it on the wire, while an allowed
    // name IS applied. upstream: xattrs.c:822 receive_xattr() drops an excluded
    // received name via name_is_excluded(name, NAME_IS_XATTR, ALL_FILTERS).
    #[test]
    fn apply_xattrs_from_list_filter_drops_excluded_received() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        list.push(XattrEntry::new(test_xattr_name("keep"), b"kept".to_vec()));
        list.push(XattrEntry::new(
            test_xattr_name("skipme"),
            b"dropped".to_vec(),
        ));

        // Exclude any name containing "skip" (matches on the local xattr name,
        // which carries the `user.` prefix on Linux and is bare elsewhere).
        let filter = |name: &str| !name.contains("skip");
        apply_xattrs_from_list(&file, &list, false, None, Some(&filter), None)
            .expect("apply xattrs");

        // The allowed attr is applied.
        assert_eq!(
            read_attribute(&file, &test_xattr_name("keep"), false)
                .expect("read")
                .expect("keep"),
            b"kept"
        );
        // The excluded attr the sender transmitted is NOT applied.
        assert!(
            read_attribute(&file, &test_xattr_name("skipme"), false)
                .expect("read")
                .is_none(),
            "filtered received xattr must not land on the destination"
        );
    }

    // A destination xattr the filter excludes must be preserved even when it is
    // absent from the received list, while a non-excluded stale attr is still
    // removed. upstream: xattrs.c:1026 rsync_xal_set() skips the removal of an
    // excluded destination name.
    #[test]
    fn apply_xattrs_from_list_filter_preserves_excluded_dest_attr() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Excluded pre-existing attr (not in the received list).
        let excluded = test_xattr_name("skipme");
        write_attribute(&file, &excluded, b"local", false).expect("write excluded");
        // Non-excluded stale attr (not in the received list).
        let stale = test_xattr_name("stale");
        write_attribute(&file, &stale, b"old", false).expect("write stale");

        let mut list = XattrList::new();
        list.push(XattrEntry::new(test_xattr_name("keep"), b"kept".to_vec()));

        let filter = |name: &str| !name.contains("skip");
        apply_xattrs_from_list(&file, &list, false, None, Some(&filter), None)
            .expect("apply xattrs");

        // Excluded destination attr is preserved.
        assert_eq!(
            read_attribute(&file, &excluded, false)
                .expect("read")
                .expect("excluded preserved"),
            b"local"
        );
        // Non-excluded stale attr is removed.
        assert!(
            read_attribute(&file, &stale, false)
                .expect("read")
                .is_none(),
            "non-excluded stale dest xattr must be removed"
        );
        // The received attr is applied.
        assert_eq!(
            read_attribute(&file, &test_xattr_name("keep"), false)
                .expect("read")
                .expect("keep"),
            b"kept"
        );
    }

    #[test]
    fn apply_xattrs_from_list_removes_stale_dest_attrs() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Pre-set an xattr on the destination that is not in the source list
        let stale = test_xattr_name("stale");
        write_attribute(&file, &stale, b"old", false).expect("write stale");

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            test_xattr_name("kept"),
            b"new_value".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false, None, None, None).expect("apply xattrs");

        // Stale attr should be removed
        assert!(
            read_attribute(&file, &stale, false)
                .expect("read")
                .is_none()
        );

        // New attr should be present
        let kept = test_xattr_name("kept");
        assert_eq!(
            read_attribute(&file, &kept, false)
                .expect("read")
                .expect("kept"),
            b"new_value"
        );
    }

    /// Without a basis file an abbreviated entry cannot be resolved, so its
    /// value is not written - but a full entry in the same list still applies.
    /// upstream: xattrs.c:970-975 rsync_xal_set() takes the `still_abbrev` path
    /// when it cannot recover the value; a receiver with no local copy keeps the
    /// attribute unset rather than writing a checksum as if it were the datum.
    #[test]
    fn apply_xattrs_from_list_no_basis_leaves_abbreviated_unset() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        // Abbreviated entry - only carries a checksum, not the full value.
        list.push(XattrEntry::abbreviated(
            test_xattr_name("abbrev"),
            vec![0xAA; 16],
            100,
        ));
        list.push(XattrEntry::new(
            test_xattr_name("full"),
            b"full_value".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false, None, None, None).expect("apply xattrs");

        assert!(
            read_attribute(&file, &test_xattr_name("abbrev"), false)
                .expect("read")
                .is_none(),
            "an abbreviated value must never be written as its raw checksum"
        );
        assert_eq!(
            read_attribute(&file, &test_xattr_name("full"), false)
                .expect("read")
                .expect("full"),
            b"full_value"
        );
    }

    /// An abbreviated entry references a large value the sender expects the
    /// receiver to already hold on the basis (fnamecmp) file. The receiver must
    /// re-read that value and, when its length and digest match the reference,
    /// apply the full value to the destination - byte-identical to a transfer
    /// that carried the value in full. Losing this resolution silently drops
    /// every large (>32-byte) xattr on interop with an upstream sender that
    /// abbreviates it. upstream: xattrs.c:964-1009 rsync_xal_set().
    #[test]
    fn apply_xattrs_from_list_resolves_abbreviated_against_basis() {
        use protocol::xattr::compute_xattr_checksum;

        let dir = tempdir().expect("create temp dir");
        let basis = dir.path().join("basis.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&basis, "basis-content").expect("write basis");
        fs::write(&dest, "dest-content").expect("write dest");

        if !xattrs_supported(&basis) || !xattrs_supported(&dest) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // A value longer than MAX_FULL_DATUM (32) is what upstream abbreviates.
        let name = test_xattr_name("large");
        let value = vec![0x5Au8; 100];
        write_attribute(&basis, &name, &value, false).expect("seed basis xattr");

        // Build the abbreviated wire entry exactly as the sender would: the
        // digest is the (unseeded, proto 30-32) MD5 of the full value and
        // datum_len records the original length.
        let digest = compute_xattr_checksum(&value, 0).to_vec();
        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated(name.clone(), digest, value.len()));

        apply_xattrs_from_list(&dest, &list, false, Some(&basis), None, None)
            .expect("apply xattrs");

        // The destination must carry the fully resolved value, identical to a
        // full-list transfer.
        assert_eq!(
            read_attribute(&dest, &name, false)
                .expect("read")
                .expect("resolved large xattr"),
            value
        );
    }

    /// A digest that matches no basis value must not be applied: a wrong length
    /// or a mismatched checksum takes upstream's `still_abbrev` path, leaving the
    /// destination unchanged rather than copying an unrelated basis value.
    #[test]
    fn apply_xattrs_from_list_abbreviated_mismatch_not_applied() {
        let dir = tempdir().expect("create temp dir");
        let basis = dir.path().join("basis.txt");
        let dest = dir.path().join("dest.txt");
        fs::write(&basis, "basis-content").expect("write basis");
        fs::write(&dest, "dest-content").expect("write dest");

        if !xattrs_supported(&basis) || !xattrs_supported(&dest) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let name = test_xattr_name("large");
        // The basis holds a value whose digest will NOT match the reference.
        write_attribute(&basis, &name, &[0x11u8; 100], false).expect("seed basis xattr");

        let mut list = XattrList::new();
        list.push(XattrEntry::abbreviated(name.clone(), vec![0xAAu8; 16], 100));

        apply_xattrs_from_list(&dest, &list, false, Some(&basis), None, None)
            .expect("apply xattrs");

        assert!(
            read_attribute(&dest, &name, false).expect("read").is_none(),
            "a digest mismatch must not copy an unrelated basis value"
        );
    }

    #[test]
    fn apply_xattrs_from_list_empty_list_clears_all() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Pre-set xattrs on destination
        let attr = test_xattr_name("existing");
        write_attribute(&file, &attr, b"value", false).expect("write existing");

        let list = XattrList::new();
        apply_xattrs_from_list(&file, &list, false, None, None, None).expect("apply empty list");

        // All permitted xattrs should be removed
        assert!(read_attribute(&file, &attr, false).expect("read").is_none());
    }

    #[test]
    fn apply_xattrs_from_list_overwrites_existing() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr = test_xattr_name("shared");
        write_attribute(&file, &attr, b"old_value", false).expect("write old");

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            test_xattr_name("shared"),
            b"new_value".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false, None, None, None).expect("apply xattrs");

        assert_eq!(
            read_attribute(&file, &attr, false)
                .expect("read")
                .expect("shared"),
            b"new_value"
        );
    }

    #[test]
    fn apply_xattrs_from_list_with_empty_value() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        list.push(XattrEntry::new(test_xattr_name("empty_val"), b"".to_vec()));

        apply_xattrs_from_list(&file, &list, false, None, None, None).expect("apply xattrs");

        let attr = test_xattr_name("empty_val");
        let value = read_attribute(&file, &attr, false)
            .expect("read")
            .expect("empty_val should exist");
        assert!(value.is_empty());
    }

    /// The whole point of the confined pin: with a `confine_root` supplied,
    /// a parent component flipped to a symlink pointing OUT of the tree must
    /// not carry the attribute to the outside file.
    ///
    /// A path-based `lsetxattr` follows parent symlinks (it declines only the
    /// leaf), so this is exactly the redirect upstream's held-fd model
    /// closes. upstream: rsync.c:573-599 + xattrs.c:386-390.
    #[cfg(all(unix, feature = "xattr"))]
    #[test]
    fn a_flipped_parent_does_not_carry_the_attribute_outside_the_confine_root() {
        let tmp = tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir(&root).expect("mkdir root");
        std::fs::create_dir(&outside).expect("mkdir outside");
        let victim = outside.join("victim");
        std::fs::write(&victim, b"outside").expect("write victim");
        std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant escaping parent");

        let mut list = XattrList::default();
        list.push(XattrEntry::new(b"user.marker".to_vec(), b"PWNED".to_vec()));

        // The write is skipped, not redirected: upstream's `xattr_refuse`.
        // Whether the call reports Ok is upstream's choice too - it drops the
        // attribute and carries on - so the assertion that matters is on the
        // SENTINEL, not the return value.
        let _ = apply_xattrs_from_list(
            &root.join("sub/victim"),
            &list,
            false,
            None,
            None,
            Some(&root),
        );

        let landed = ::xattr::list(&victim)
            .expect("list victim")
            .any(|name| name == std::ffi::OsStr::new("user.marker"));
        assert!(
            !landed,
            "the attacker-chosen xattr was written onto a file OUTSIDE the confine root"
        );
    }

    /// Non-vacuity companion: with the SAME fixture shape but a legitimate
    /// in-tree relative parent symlink, the attribute must still be applied.
    /// Without this the test above would pass for a batch that simply never
    /// writes anything.
    #[cfg(all(unix, feature = "xattr"))]
    #[test]
    fn an_in_tree_parent_symlink_still_receives_the_attribute() {
        let tmp = tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        std::fs::create_dir(root.join("real")).expect("mkdir real");
        let target = root.join("real/data");
        std::fs::write(&target, b"payload").expect("write");
        std::os::unix::fs::symlink("real", root.join("sub")).expect("plant in-tree parent");

        let mut list = XattrList::default();
        list.push(XattrEntry::new(b"user.marker".to_vec(), b"OK".to_vec()));

        if apply_xattrs_from_list(
            &root.join("sub/data"),
            &list,
            false,
            None,
            None,
            Some(&root),
        )
        .is_err()
        {
            // A filesystem without user-xattr support cannot exercise this.
            return;
        }

        let landed = ::xattr::list(&target)
            .expect("list target")
            .any(|name| name == std::ffi::OsStr::new("user.marker"));
        assert!(
            landed,
            "an in-tree relative parent symlink must still resolve, or the escape \
             test above proves nothing"
        );
    }
}
