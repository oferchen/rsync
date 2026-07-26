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
    /// upstream: `xattrs.c:237` - `int user_only = am_sender ? 0 : !am_root;`
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
    xattr_diff(sender, &dest, dest_opts.checksum_seed)
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
    /// upstream: xattrs.c:237 - `int user_only = am_sender ? 0 : !am_root;`
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
    /// upstream: xattrs.c:262 - `am_sender && preserve_xattrs < 2`
    #[test]
    fn is_sender_reports_the_upstream_am_sender_global() {
        assert!(XattrRole::Sender.is_sender());
        assert!(!XattrRole::Generator.is_sender());
    }
}
