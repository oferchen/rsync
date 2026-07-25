//! Collection options for reading a path's extended attributes into a
//! wire-format `XattrList`.
//!
//! Upstream reads xattrs through a single function, `xattrs.c:rsync_xal_get()`,
//! whose behaviour is steered by four globals: `am_sender`, `am_root`,
//! `preserve_xattrs`, and `saw_xattr_filter`. This crate has no globals, so the
//! same inputs travel as an explicit options record rather than a long
//! positional parameter list.

/// Inputs that steer sender-side xattr collection.
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
    /// Resolve a symlink before reading (`getxattr`) instead of reading the
    /// link itself (`lgetxattr`).
    pub follow_symlinks: bool,
    /// Whether the reading process is privileged, which is what lets the
    /// `system.*` namespace reach the wire.
    pub am_root: bool,
    /// Xattr preservation level: 1 for `-X`, 2 for `-XX`.
    ///
    /// Upstream gates the `rsync.%FOO` strip on `am_sender && preserve_xattrs
    /// < 2`. There is no separate `am_sender` field here: a caller that is not
    /// the sender (upstream's generator comparing the destination) passes level
    /// 2, which reproduces `am_sender == 0` exactly, because the strip is the
    /// only place the role is consulted.
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

impl Default for XattrSendOptions<'_> {
    /// A plain single `-X` read: unprivileged, no fake-super, no filter.
    ///
    /// The level defaults to 1 rather than 0 so that a caller which forgets to
    /// set it gets the conservative `-X` behaviour (strip `rsync.%FOO`) instead
    /// of silently transmitting the fake-super store.
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            am_root: false,
            preserve_xattrs: 1,
            fake_super: false,
            filter: None,
            checksum_seed: 0,
        }
    }
}
