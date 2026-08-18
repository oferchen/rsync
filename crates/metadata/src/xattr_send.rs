//! Collection options for reading a path's extended attributes into a
//! wire-format `XattrList`.
//!
//! Upstream reads xattrs through a single function, `xattrs.c:rsync_xal_get()`,
//! whose behaviour is steered by four globals: `am_sender`, `am_root`,
//! `preserve_xattrs`, and `saw_xattr_filter`. This crate has no globals, so the
//! same inputs travel as an explicit options record rather than a long
//! positional parameter list.
//!
//! The module also hosts [`dest_xattrs_differ`], the single destination-side
//! comparison behind the itemize `x` column, shared by the network receiver and
//! the local-copy executor.

use std::path::Path;

use protocol::xattr::{XattrList, xattr_diff};

/// Which side of the transfer is collecting the attributes.
///
/// Upstream reads this from the `am_sender` global in two places:
/// `xattrs.c:237` (`user_only`) and `xattrs.c:262` (the `rsync.%FOO` strip).
/// Both consult the role, so it cannot be folded into another field.
///
/// The enum deliberately has no `Default`: the two roles select different
/// namespaces for a non-root process, so a call site that omits the role would
/// silently collect the wrong set of attributes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XattrRole {
    /// Upstream's `am_sender != 0`: the file-list builder describing a source
    /// file to the peer.
    Sender,
    /// Upstream's `am_sender == 0`: the generator reading the destination in
    /// order to diff it against the sender's list, and every other
    /// receiver-side read.
    Generator,
}

impl XattrRole {
    /// Upstream's `user_only` for this role at the given privilege level.
    ///
    /// upstream: `xattrs.c:249` - `int user_only = am_sender ? 0 : !am_root;`
    /// The sender never restricts to the `user.*` namespace; a non-root
    /// generator always does.
    #[must_use]
    pub fn user_only(self, am_root: bool) -> bool {
        match self {
            Self::Sender => false,
            Self::Generator => !am_root,
        }
    }

    /// Whether this role is upstream's `am_sender`.
    #[must_use]
    pub fn is_sender(self) -> bool {
        matches!(self, Self::Sender)
    }
}

/// Inputs that steer xattr collection for the wire.
///
/// Mirrors the globals consulted by upstream `xattrs.c:rsync_xal_get()`
/// (lines 231-300).
///
/// # Upstream Reference
///
/// - `xattrs.c:237` - `int user_only = am_sender ? 0 : !am_root;`
/// - `xattrs.c:250-257` - the `x`-modifier filter branch and the namespace
///   branch are mutually exclusive
/// - `xattrs.c:260-267` - the `rsync.%FOO` internal-attribute gate
#[derive(Clone, Copy)]
pub struct XattrSendOptions<'a> {
    /// Which side is reading (upstream's `am_sender`).
    pub role: XattrRole,
    /// Resolve a symlink before reading (`getxattr`) instead of reading the
    /// link itself (`lgetxattr`).
    pub follow_symlinks: bool,
    /// Whether the reading process is privileged (upstream's `am_root`).
    ///
    /// It decides two things: whether a generator read is confined to the
    /// `user.*` namespace (`xattrs.c:237`), and whether the `system.*`
    /// namespace can reach the wire at all.
    pub am_root: bool,
    /// Xattr preservation level: 1 for `-X`, 2 for `-XX`.
    ///
    /// Upstream gates the `rsync.%FOO` strip on `am_sender && preserve_xattrs
    /// < 2`, so this only matters for [`XattrRole::Sender`].
    pub preserve_xattrs: u8,
    /// Whether `--fake-super` is active (upstream's `am_root < 0`).
    ///
    /// Independent of role: it suppresses the three fake-super store
    /// attributes (`rsync.%stat`, `rsync.%aacl`, `rsync.%dacl`) even at
    /// preservation level 2, because those describe the *local* fake-super
    /// state rather than the file's own metadata.
    pub fake_super: bool,
    /// Optional `x`-modifier filter predicate. A name for which it returns
    /// `false` is not collected.
    ///
    /// When this is `Some`, upstream skips the namespace test entirely
    /// (`xattrs.c:250-257`: the namespace check is the `else` of the filter
    /// check), so the filter alone decides.
    pub filter: Option<&'a dyn Fn(&str) -> bool>,
    /// Seed for the digest that abbreviates values over `MAX_FULL_DATUM`.
    pub checksum_seed: i32,
}

impl XattrSendOptions<'_> {
    /// Base options for `role`: a plain single `-X` read by an unprivileged
    /// process, with no fake-super and no filter.
    ///
    /// Intended as the tail of a struct-update expression so that every call
    /// site names its role. There is deliberately no `Default`, because the
    /// role has no safe default.
    ///
    /// The level starts at 1 rather than 0 so that a caller which does not set
    /// it gets the conservative `-X` behaviour (strip `rsync.%FOO`) instead of
    /// silently transmitting the fake-super store.
    #[must_use]
    pub fn new(role: XattrRole) -> Self {
        Self {
            role,
            follow_symlinks: false,
            am_root: false,
            preserve_xattrs: 1,
            fake_super: false,
            filter: None,
            checksum_seed: 0,
        }
    }

    /// Upstream's `user_only` for these options (`xattrs.c:237`).
    #[must_use]
    pub fn user_only(&self) -> bool {
        self.role.user_only(self.am_root)
    }
}

/// Whether `destination`'s extended attributes differ from the `sender` set,
/// for the `ITEM_REPORT_XATTR` (`x`) column of `--itemize-changes`.
///
/// Mirrors upstream `generator.c:566-572`: with `preserve_xattrs` the generator
/// reads the destination's current attributes (`get_xattr`) and compares them
/// against the sender's list (`xattr_diff`). `dest_opts` must therefore describe
/// a *generator* read - [`XattrRole::Generator`] - so `user_only`
/// (`xattrs.c:237`) confines an unprivileged process to `user.*` and a
/// namespace it could never store cannot raise the column.
///
/// Callers must invoke this before applying the sender's attributes, so the
/// comparison sees the pre-transfer destination. A destination that cannot be
/// read (missing, or a filesystem without xattr support) reports no difference:
/// upstream's `get_xattr` failure path likewise leaves the receiver list empty
/// rather than inventing a change, and a missing destination is itemized as
/// `ITEM_IS_NEW` where every column renders `+`.
///
/// Both the network receiver (sender list decoded from the flist) and the
/// local-copy executor (sender list read from the source with
/// [`XattrRole::Sender`]) route through here, so there is exactly one
/// destination-side comparison in the codebase.
#[must_use]
pub fn dest_xattrs_differ(
    sender: &XattrList,
    destination: &Path,
    dest_opts: &XattrSendOptions<'_>,
) -> bool {
    debug_assert!(
        !dest_opts.role.is_sender(),
        "the itemize xattr comparison reads the destination as the generator",
    );
    let Ok(dest) = crate::read_xattrs_for_wire(destination, dest_opts) else {
        return false;
    };

    // On macOS the kernel attaches provenance metadata (com.apple.provenance)
    // to executables and quarantined files on its own, independently of the
    // file's user data, and each inode receives its own value. It reaches this
    // wire-format comparison as `user.com.apple.provenance`. Diffing it
    // dest-vs-source would light the itemize `x` column (and fail quick-check)
    // for two otherwise identical files on every run, so it is screened out of
    // the difference comparison only - never from an explicit xattr transfer.
    // Upstream rsync is Linux-centric and has no provenance concept, so this is
    // an oc macOS-platform-fidelity screen rather than an upstream divergence.
    #[cfg(target_os = "macos")]
    let differ = {
        let sender = without_macos_auto_xattrs(sender);
        let dest = without_macos_auto_xattrs(&dest);
        xattr_diff(&sender, &dest, dest_opts.checksum_seed)
    };
    #[cfg(not(target_os = "macos"))]
    let differ = xattr_diff(sender, &dest, dest_opts.checksum_seed);
    differ
}

/// macOS extended attributes the kernel attaches to an inode on its own,
/// independently of the file's user data.
///
/// This is the single, authoritative list of names screened out of the
/// destination-vs-source difference that drives the itemize `x` column and
/// quick-check. Extend it here if another OS-managed attribute is found to
/// auto-attach on write.
///
/// `com.apple.quarantine` is deliberately absent: it is genuine Gatekeeper
/// metadata that rsync transfers, is not attached by a plain write, and
/// screening it would hide real changes. `com.apple.provenance` is the one
/// confirmed offender - the kernel re-attaches its own per-inode value to any
/// executable, so a freshly written destination never matches the source.
///
/// Names are stored without the `user.` prefix that the macOS wire encoding
/// (`protocol::xattr::local_to_wire`) prepends; [`is_macos_auto_xattr`] strips
/// it before matching.
#[cfg(target_os = "macos")]
const MACOS_AUTO_XATTRS: &[&[u8]] = &[b"com.apple.provenance"];

/// Whether `wire_name` names a macOS OS-attached attribute from
/// [`MACOS_AUTO_XATTRS`], tolerating the `user.` wire prefix.
#[cfg(target_os = "macos")]
fn is_macos_auto_xattr(wire_name: &[u8]) -> bool {
    let local = wire_name.strip_prefix(b"user.").unwrap_or(wire_name);
    MACOS_AUTO_XATTRS.contains(&local)
}

/// Returns `list` without any [`MACOS_AUTO_XATTRS`] entry, for the itemize
/// difference comparison. Both the sender list and the freshly read
/// destination list pass through here so an OS-attached attribute present on
/// only one side cannot report a change.
#[cfg(target_os = "macos")]
fn without_macos_auto_xattrs(list: &XattrList) -> XattrList {
    list.iter()
        .filter(|entry| !is_macos_auto_xattr(entry.name()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{XattrRole, XattrSendOptions};

    /// `user_only` is a function of the role first and privilege second.
    ///
    /// Reading it off `am_root` alone would confine an unprivileged *sender*
    /// to `user.*` and drop SELinux labels from the wire; reading it off the
    /// role alone would let a root generator ignore namespaces it can store.
    ///
    /// upstream: xattrs.c:249 - `int user_only = am_sender ? 0 : !am_root;`
    #[test]
    fn user_only_mirrors_the_upstream_ternary() {
        assert!(!XattrRole::Sender.user_only(false));
        assert!(!XattrRole::Sender.user_only(true));
        assert!(XattrRole::Generator.user_only(false));
        assert!(!XattrRole::Generator.user_only(true));
    }

    /// The options record derives `user_only` from the fields it carries, so a
    /// call site cannot set the privilege level and forget the role.
    #[test]
    fn options_derive_user_only_from_role_and_privilege() {
        let generator = XattrSendOptions::new(XattrRole::Generator);
        assert!(generator.user_only(), "unprivileged generator");
        assert!(
            !XattrSendOptions {
                am_root: true,
                ..generator
            }
            .user_only(),
            "root generator",
        );
        assert!(!XattrSendOptions::new(XattrRole::Sender).user_only());
    }

    /// The role also drives the `rsync.%FOO` strip, which is why it is a field
    /// rather than something inferred from the preservation level.
    ///
    /// upstream: xattrs.c:274 - `am_sender && preserve_xattrs < 2`
    #[test]
    fn is_sender_reports_the_upstream_am_sender_global() {
        assert!(XattrRole::Sender.is_sender());
        assert!(!XattrRole::Generator.is_sender());
    }
}

#[cfg(all(test, target_os = "macos", feature = "xattr"))]
mod macos_provenance_tests {
    use super::{XattrRole, XattrSendOptions, dest_xattrs_differ};
    use protocol::xattr::{XattrEntry, XattrList};
    use std::fs;
    use tempfile::tempdir;

    /// macOS attaches `com.apple.provenance` to executables and quarantined
    /// files on its own, and each inode receives its own value. Before the fix
    /// the generator's dest-vs-source xattr diff counted that OS-attached
    /// attribute as a real change and lit the itemize `x` column for two
    /// otherwise identical files on every run. It must be ignored from the
    /// difference comparison, while a genuine user-xattr difference is still
    /// reported. This test fails on pre-fix code, where the differing
    /// provenance value alone flips the comparison to "differ".
    #[test]
    fn provenance_is_excluded_from_the_dest_diff_but_real_changes_are_not() {
        let dir = tempdir().expect("temp dir");
        let dest = dir.path().join("bin");
        fs::write(&dest, "content").expect("write dest");

        // `xattr::set` writes the bare local name; `read_xattrs_for_wire` then
        // wire-encodes it to `user.<name>` exactly as the sender does, so the
        // dest list carries `user.com.apple.provenance` and `user.shared`.
        if xattr::set(&dest, "com.apple.provenance", b"dest-value").is_err() {
            eprintln!("cannot set com.apple.provenance here, skipping");
            return;
        }
        xattr::set(&dest, "shared", b"v").expect("set shared attr");

        let opts = XattrSendOptions::new(XattrRole::Generator);

        // Sender list as it arrives on the wire: a *different* provenance value
        // (its own inode's) but the same genuine user attribute.
        let matching = XattrList::with_entries(vec![
            XattrEntry::new(b"user.com.apple.provenance".to_vec(), b"src-value".to_vec()),
            XattrEntry::new(b"user.shared".to_vec(), b"v".to_vec()),
        ]);
        assert!(
            !dest_xattrs_differ(&matching, &dest, &opts),
            "a differing com.apple.provenance must not report an xattr change",
        );

        // A genuine user-attribute difference is still detected.
        let changed = XattrList::with_entries(vec![
            XattrEntry::new(b"user.com.apple.provenance".to_vec(), b"src-value".to_vec()),
            XattrEntry::new(b"user.shared".to_vec(), b"different".to_vec()),
        ]);
        assert!(
            dest_xattrs_differ(&changed, &dest, &opts),
            "a real user-xattr difference must still report an xattr change",
        );
    }
}

/// The two xattr-name filters [`sync_xattrs`] consults, one per upstream side.
///
/// A local copy is upstream's forked sender + generator, and the two halves of
/// this function are exactly that fork:
///
/// - reading the SOURCE names is the sender's `rsync_xal_get()`
///   (xattrs.c:250-267), which runs with `am_sender = 1`;
/// - deleting the extraneous DESTINATION names is the generator's prune in
///   `rsync_xal_set()` (xattrs.c:1078-1082), which runs with `am_sender = 0`.
///
/// `rule_matches()` elides a side-flagged rule before it consults the pattern
/// (exclude.c:1010), so the two halves genuinely see different rule sets: an
/// `H`/`S` rule shapes only the source read and a `P`/`R` rule only the
/// destination prune. Sharing one predicate across both collapses that
/// distinction - and measured against rsync 3.5.0 it is observable, e.g.
/// `--filter='Hx user.foo'` must not copy the attribute while
/// `--filter='Px user.foo'` must not delete an extraneous one.
///
/// [`Default`] leaves both sides unfiltered, which is `-X` with no `x` rules.
#[derive(Clone, Copy, Default)]
pub struct XattrSyncFilters<'a> {
    /// Consulted with each name read from the source, upstream's sender side.
    pub source: Option<&'a dyn Fn(&str) -> bool>,
    /// Consulted with each extraneous destination name, upstream's generator side.
    pub destination: Option<&'a dyn Fn(&str) -> bool>,
}

impl<'a> XattrSyncFilters<'a> {
    /// Applies one predicate to both sides.
    ///
    /// Correct only where the caller has no side distinction to make - a rule
    /// set with no side-flagged rules, or a test fixture. Production callers
    /// that own a filter chain should build the two sides separately.
    #[must_use]
    pub const fn uniform(filter: &'a dyn Fn(&str) -> bool) -> Self {
        Self {
            source: Some(filter),
            destination: Some(filter),
        }
    }
}

impl std::fmt::Debug for XattrSyncFilters<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XattrSyncFilters")
            .field("source", &self.source.is_some())
            .field("destination", &self.destination.is_some())
            .finish()
    }
}
