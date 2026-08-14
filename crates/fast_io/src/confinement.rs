//! Path-confinement activation: who is confined, where the boundary sits, and
//! which opens the boundary applies to.
//!
//! The per-component confined walk lives in
//! [`dir_sandbox`](crate::dir_sandbox); this module answers the question that
//! precedes it - *should this process confine this open at all, and against
//! what root?*
//!
//! # Upstream is four decisions, not one
//!
//! Upstream spreads the activation question across four independent pieces of
//! state, and conflating any two of them changes behaviour:
//!
//! | Question | Upstream | Here |
//! |---|---|---|
//! | Is the hardening opted out of? | `symlink_optout_allowed()` | [`Activation::optout_allowed`](crate::confinement::Activation::optout_allowed) |
//! | Is path resolution hardened at all? | `secure_relpath_active()` | [`Activation::hardened`](crate::confinement::Activation::hardened) |
//! | What root must a path stay under? | `confinement_root()` | [`Activation::root`](crate::confinement::Activation::root) |
//! | Is *this* open subject to the root? | `operator_path_resolve` | [`PathKind`](crate::confinement::PathKind) |
//!
//! The first three are properties of the session and live on [`Activation`](crate::confinement::Activation).
//! The fourth is a property of the individual call.
//!
//! # Why `PathKind` is a parameter and not session state
//!
//! Upstream's `operator_path_resolve` is a mutable global toggled with
//! save/restore around roughly fifty call sites. That shape makes the
//! *effective* policy depend on control flow, so a missed restore silently
//! widens or narrows confinement somewhere unrelated. Passing the kind as an
//! argument makes every site state its own answer and makes an unstated one a
//! compile error. The semantics are identical; only the mechanism differs.
//!
//! # Upstream Reference
//!
//! - `syscall.c:100-114` - `secure_relpath_active()`
//! - `syscall.c:122-127` - `symlink_optout_allowed()`
//! - `syscall.c:136-144` - `confinement_root()`
//! - `syscall.c:197-240` - `abspath_outside_confinement()`
//! - `syscall.c:552` - `int operator_path_resolve = 0;`

use std::path::{Path, PathBuf};

/// Which end of the transfer this process is.
///
/// Upstream spells this `am_sender`. The generator is part of the receiving
/// side and runs with `am_sender = 0`, so it takes the
/// [`Receiver`](Role::Receiver) arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Reads source files and sends them (`am_sender = 1`).
    Sender,
    /// Writes destination files; also the generator (`am_sender = 0`).
    Receiver,
}

/// The `insecure links` value read from a daemon's own module configuration.
///
/// Distinct from [`LocalInsecureLinks`] so the two cannot be assigned to one
/// another. A client cannot switch off a daemon's confinement, and the daemon
/// drops a connection that sends `--insecure-links`, so a peer-supplied value
/// must never reach the daemon arm of [`Activation::optout_allowed`]. A single
/// shared `bool` makes that a rule someone has to remember; two types make it a
/// compile error. The constructor names the source, so a call site that lies
/// about provenance has to say so out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModuleInsecureLinks(bool);

impl ModuleInsecureLinks {
    /// Record the `insecure links` directive of the module being served.
    pub const fn from_module_config(enabled: bool) -> Self {
        Self(enabled)
    }

    /// The recorded value.
    pub const fn get(self) -> bool {
        self.0
    }
}

/// The local `--insecure-links` command-line flag - upstream `insecure_links`.
///
/// See [`ModuleInsecureLinks`] for why these are two types and not one `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalInsecureLinks(bool);

impl LocalInsecureLinks {
    /// Record the locally parsed `--insecure-links` flag.
    pub const fn from_local_flag(enabled: bool) -> Self {
        Self(enabled)
    }

    /// The recorded value.
    pub const fn get(self) -> bool {
        self.0
    }
}

/// The module state a daemon serves.
///
/// Mirrors the four upstream globals a daemon consults: `module_dir` /
/// `module_dirlen`, `am_chrooted`, `module_id`, and `lp_insecure_links()`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModuleState {
    /// The served module's root, the boundary a peer path must stay under.
    ///
    /// Upstream `module_dir`; an empty or absent root is upstream's
    /// `module_dirlen == 0`.
    pub root: Option<PathBuf>,
    /// Upstream `am_chrooted`.
    ///
    /// Assigned in exactly one place - `clientserver.c:988`, inside
    /// `rsync_module()`'s per-module `use chroot` handling. The daemon-level
    /// `daemon chroot` deliberately leaves it clear (`clientserver.c:1345-1357`)
    /// because that chroot confines resolution to the daemon root rather than
    /// to a module boundary, so the per-module defences must keep firing.
    pub chrooted: bool,
    /// Whether a module has been selected yet - upstream `module_id >= 0`.
    pub selected: bool,
    /// The module's `insecure links` directive - upstream
    /// `lp_insecure_links(module_id)`.
    pub insecure_links: ModuleInsecureLinks,
}

/// Whether this process is a daemon, and if so what it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// Not a daemon - upstream `am_daemon = 0`.
    NotDaemon,
    /// A daemon serving a connection - upstream `am_daemon != 0`.
    Daemon(ModuleState),
}

/// Whether a particular open is an operator- or peer-supplied path that must
/// stay under the confinement root.
///
/// Upstream's `operator_path_resolve`. Only [`Confined`](PathKind::Confined)
/// opens are refused for landing outside the root; the rest may legitimately
/// live elsewhere on the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// A path that must stay inside the tree: `--partial-dir`, `--backup-dir`,
    /// the alternate-basis family, and merge files.
    ///
    /// Upstream `operator_path_resolve = 1`.
    Confined,
    /// A path that may legitimately live outside the tree: `--log-file`, the
    /// `--*-from` family, and the daemon's lock and motd files.
    ///
    /// Upstream `operator_path_resolve = 0`.
    Ancillary,
}

/// The session-level answer to *who is confined, and against what root*.
///
/// Construct one per session and consult it at each open. It carries no file
/// descriptors and performs no I/O; it is pure policy, so it compiles and is
/// testable on every target including Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// Which end of the transfer this process is.
    pub role: Role,
    /// Daemon state, including the served module when there is one.
    pub daemon: DaemonState,
    /// The local `--insecure-links` flag - upstream `insecure_links`.
    ///
    /// Consulted only when [`daemon`](Self::daemon) is
    /// [`NotDaemon`](DaemonState::NotDaemon): a peer cannot switch off a
    /// daemon's confinement, so a forwarded flag is structurally inert there.
    pub insecure_links: LocalInsecureLinks,
    /// The `--confine-root` directory - upstream `confine_root`.
    ///
    /// Ignored by a daemon, which uses its module root instead: the option
    /// arrives in a peer-supplied argv, so honouring it could only loosen the
    /// module boundary.
    pub confine_root: Option<PathBuf>,
}

impl Activation {
    /// Whether the symlink confinement is opted out of.
    ///
    /// A daemon reads only its module's `insecure links` directive; a
    /// non-daemon reads the local `--insecure-links` flag.
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:122-127` - `symlink_optout_allowed()`
    pub fn optout_allowed(&self) -> bool {
        match &self.daemon {
            DaemonState::Daemon(module) => module.selected && module.insecure_links.get(),
            DaemonState::NotDaemon => self.insecure_links.get(),
        }
    }

    /// Whether path resolution must be hardened against parent-component
    /// symlink races.
    ///
    /// Hardens every non-chrooted receiver - a chroot is its own confinement -
    /// and every daemon. The sender is excluded when it is not a daemon so it
    /// still follows `--copy-links` symlinks. A daemon chroot with an inner
    /// module boundary stays hardened because the kernel chroot confines the
    /// outer path, not the inner module.
    ///
    /// The chroot exception is not a corner case: a chrooted daemon that still
    /// has an inner module boundary is the `/./` inner-module escape
    /// (CVE-2026-53793). Standing the resolver down there because "a chroot is
    /// its own confinement" is exactly the bug.
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:100-114` - `secure_relpath_active()`
    pub fn hardened(&self) -> bool {
        if self.optout_allowed() {
            return false;
        }
        if let DaemonState::Daemon(module) = &self.daemon {
            if module.chrooted && module_root_len(module) != 0 {
                return true;
            }
        }
        !self.chrooted() && (self.is_daemon() || self.role == Role::Receiver)
    }

    /// The root an operator- or peer-supplied path must stay under, or `None`
    /// when nothing is confined.
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:136-144` - `confinement_root()`
    pub fn root(&self) -> Option<&Path> {
        match &self.daemon {
            DaemonState::Daemon(module) => module.root.as_deref(),
            DaemonState::NotDaemon => self.confine_root.as_deref(),
        }
    }

    /// Whether an already-resolved absolute path lands outside the confinement
    /// root and must be refused.
    ///
    /// A path that is an *ancestor* of the root is not outside it - an absolute
    /// walk passes through `/`, `/srv`, ... on the way down, and those are
    /// not-yet-arrived rather than diverged.
    ///
    /// Upstream compares raw bytes with a manual `'/'`-or-NUL boundary test;
    /// this compares whole components, which is the same rule for the
    /// normalised absolute paths both sides feed it and cannot mistake
    /// `/srv/data-evil` for a child of `/srv/data`.
    ///
    /// The `/proc/self/fd/N` pin spelling is deliberately not handled here.
    /// A pin must be resolved to its target *before* this is consulted, so
    /// that what gets judged is where the open would land rather than how it
    /// was spelled.
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:197-240` - `abspath_outside_confinement()`
    pub fn outside_root(&self, abspath: &Path, kind: PathKind) -> bool {
        let Some(root) = self.root() else {
            return false;
        };
        // upstream: `rootlen <= 1` - a root of "/" (or none) confines nothing.
        if root.parent().is_none() || root.as_os_str().is_empty() {
            return false;
        }
        if abspath.starts_with(root) {
            return false;
        }
        // An empty path, or an ancestor of the root: still descending.
        if abspath.as_os_str().is_empty() || root.starts_with(abspath) {
            return false;
        }
        kind == PathKind::Confined
    }

    fn is_daemon(&self) -> bool {
        matches!(self.daemon, DaemonState::Daemon(_))
    }

    fn chrooted(&self) -> bool {
        match &self.daemon {
            DaemonState::Daemon(module) => module.chrooted,
            DaemonState::NotDaemon => false,
        }
    }
}

/// Upstream's `module_dirlen`: zero when the module root is absent or empty.
fn module_root_len(module: &ModuleState) -> usize {
    module
        .root
        .as_deref()
        .map_or(0, |root| root.as_os_str().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn not_daemon(role: Role) -> Activation {
        Activation {
            role,
            daemon: DaemonState::NotDaemon,
            insecure_links: LocalInsecureLinks::default(),
            confine_root: None,
        }
    }

    fn daemon(role: Role, module: ModuleState) -> Activation {
        Activation {
            role,
            daemon: DaemonState::Daemon(module),
            insecure_links: LocalInsecureLinks::default(),
            confine_root: None,
        }
    }

    fn module_at(root: &str) -> ModuleState {
        ModuleState {
            root: Some(PathBuf::from(root)),
            chrooted: false,
            selected: true,
            insecure_links: ModuleInsecureLinks::default(),
        }
    }

    /// One reachable combination of upstream's five inputs, with the value
    /// `secure_relpath_active()` produces for it.
    struct Row {
        daemon: bool,
        chrooted: bool,
        role: Role,
        /// upstream `module_dirlen != 0`
        module_root: bool,
        optout: bool,
        expect: bool,
    }

    /// Every combination of upstream's five inputs that this model can
    /// represent, with the expected `hardened()` value.
    ///
    /// Upstream's inputs are five globals, so the C truth table has 32 rows.
    /// Twelve of them are unreachable here **by construction**: `am_chrooted`
    /// and `module_dirlen` live inside [`DaemonState::Daemon`], so a non-daemon
    /// cannot be chrooted or carry a module root. Upstream relies on
    /// `rsync_module()` being the only writer of `am_chrooted` to keep those
    /// states from arising; the type makes them unspellable. The 20 rows below
    /// are therefore the whole space, not a sample.
    ///
    /// The rows that matter most are the two `daemon + chrooted + module_root`
    /// ones, which must stay hardened: that is CVE-2026-53793.
    ///
    /// `optout` here is the *effective* opt-out. `selected` (upstream
    /// `module_id >= 0`) is held true throughout, so the table does not cover
    /// the unselected-module case; `an_unselected_module_cannot_opt_out` owns
    /// that one, and it is the only test a mutation dropping the `selected`
    /// gate kills.
    const TRUTH_TABLE: &[Row] = &[
        // Not a daemon: the sender follows symlinks, the receiver is hardened.
        row(false, false, Role::Sender, false, false, false),
        row(false, false, Role::Sender, false, true, false),
        row(false, false, Role::Receiver, false, false, true),
        row(false, false, Role::Receiver, false, true, false),
        // Daemon, not chrooted: hardened in both roles.
        row(true, false, Role::Sender, false, false, true),
        row(true, false, Role::Sender, true, false, true),
        row(true, false, Role::Receiver, false, false, true),
        row(true, false, Role::Receiver, true, false, true),
        // Daemon, chrooted, no inner module boundary: the chroot suffices.
        row(true, true, Role::Sender, false, false, false),
        row(true, true, Role::Receiver, false, false, false),
        // Daemon, chrooted, WITH an inner module boundary: still hardened.
        row(true, true, Role::Sender, true, false, true),
        row(true, true, Role::Receiver, true, true, false),
        row(true, true, Role::Receiver, true, false, true),
        // The opt-out wins over every other clause, in every role.
        row(true, false, Role::Sender, false, true, false),
        row(true, false, Role::Sender, true, true, false),
        row(true, false, Role::Receiver, false, true, false),
        row(true, false, Role::Receiver, true, true, false),
        row(true, true, Role::Sender, false, true, false),
        row(true, true, Role::Sender, true, true, false),
        row(true, true, Role::Receiver, false, true, false),
    ];

    const fn row(
        daemon: bool,
        chrooted: bool,
        role: Role,
        module_root: bool,
        optout: bool,
        expect: bool,
    ) -> Row {
        Row {
            daemon,
            chrooted,
            role,
            module_root,
            optout,
            expect,
        }
    }

    fn activation_for(r: &Row) -> Activation {
        if r.daemon {
            let module = ModuleState {
                root: r.module_root.then(|| PathBuf::from("/srv/data")),
                chrooted: r.chrooted,
                selected: true,
                insecure_links: ModuleInsecureLinks::from_module_config(r.optout),
            };
            daemon(r.role, module)
        } else {
            Activation {
                role: r.role,
                daemon: DaemonState::NotDaemon,
                insecure_links: LocalInsecureLinks::from_local_flag(r.optout),
                confine_root: None,
            }
        }
    }

    /// The exhaustive table. A single inverted clause silently disables
    /// confinement at every downstream site, and this is the cheapest place to
    /// catch it.
    #[test]
    fn hardened_matches_upstream_for_every_representable_input() {
        for r in TRUTH_TABLE {
            assert!(
                r.daemon || (!r.chrooted && !r.module_root),
                "unrepresentable row: only a daemon can be chrooted or hold a module root",
            );
            let act = activation_for(r);
            assert_eq!(
                act.hardened(),
                r.expect,
                "daemon={} chrooted={} role={:?} module_root={} optout={}",
                r.daemon,
                r.chrooted,
                r.role,
                r.module_root,
                r.optout,
            );
        }
    }

    /// The table must cover the whole representable space, not a subset: 4
    /// non-daemon rows (role x optout) plus 16 daemon rows (role x chrooted x
    /// module_root x optout). Without this, deleting a row would silently
    /// shrink the sweep.
    #[test]
    fn the_truth_table_covers_every_representable_input() {
        let mut seen: Vec<(bool, bool, bool, bool, bool)> = TRUTH_TABLE
            .iter()
            .map(|r| {
                (
                    r.daemon,
                    r.chrooted,
                    matches!(r.role, Role::Sender),
                    r.module_root,
                    r.optout,
                )
            })
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), TRUTH_TABLE.len(), "duplicate rows in the table");
        assert_eq!(seen.len(), 4 + 16, "table no longer covers every input");
    }

    /// A peer-supplied value cannot reach the daemon arm, and the types say so
    /// rather than a convention saying so. This asserts the runtime rule; the
    /// structural guarantee is that `ModuleInsecureLinks` and
    /// `LocalInsecureLinks` are distinct types with no conversion between them,
    /// so the assignment that would break it does not compile.
    #[test]
    fn the_two_optout_sources_are_separate_types() {
        let from_peer = LocalInsecureLinks::from_local_flag(true);
        let from_config = ModuleInsecureLinks::from_module_config(false);
        assert!(from_peer.get());
        assert!(!from_config.get());

        let mut module = module_at("/srv/data");
        module.insecure_links = from_config;
        let mut act = daemon(Role::Receiver, module);
        act.insecure_links = from_peer;
        assert!(!act.optout_allowed());
    }

    /// upstream `secure_relpath_active()` returns `!am_chrooted && (am_daemon
    /// || !am_sender)`, so a plain local or SSH receiver is hardened with no
    /// daemon anywhere. Gating hardening on "is this a daemon" alone would
    /// leave every ordinary receiver following a planted parent symlink.
    #[test]
    fn a_non_daemon_receiver_is_hardened() {
        assert!(not_daemon(Role::Receiver).hardened());
    }

    /// The sender is deliberately excluded so `--copy-links` still follows the
    /// symlinks the operator asked it to follow.
    #[test]
    fn a_non_daemon_sender_is_not_hardened() {
        assert!(!not_daemon(Role::Sender).hardened());
    }

    /// `am_daemon` hardens the sender too - a daemon serving a module must not
    /// read through a symlink that leaves it.
    #[test]
    fn a_daemon_sender_is_hardened() {
        assert!(daemon(Role::Sender, module_at("/srv/data")).hardened());
    }

    /// A per-module `use chroot` is its own confinement, so the resolver stands
    /// down - but only when the module has no inner boundary left to defend.
    #[test]
    fn a_chrooted_daemon_without_a_module_root_is_not_hardened() {
        let module = ModuleState {
            root: None,
            chrooted: true,
            selected: true,
            insecure_links: ModuleInsecureLinks::default(),
        };
        assert!(!daemon(Role::Receiver, module).hardened());
    }

    /// The kernel chroot confines the outer path, not the inner module, so a
    /// chrooted daemon that still has a module root keeps confining.
    #[test]
    fn a_chrooted_daemon_with_a_module_root_stays_hardened() {
        let mut module = module_at("/data");
        module.chrooted = true;
        assert!(daemon(Role::Receiver, module).hardened());
    }

    /// The opt-out restores the legacy follow-any-symlink behaviour uniformly,
    /// so it disables hardening on the receiver side too - not just the sender
    /// enumeration.
    #[test]
    fn the_local_optout_disables_hardening_for_a_non_daemon() {
        let mut act = not_daemon(Role::Receiver);
        assert!(act.hardened());
        act.insecure_links = LocalInsecureLinks::from_local_flag(true);
        assert!(!act.hardened());
    }

    /// A client cannot switch off a daemon's confinement. The daemon consults
    /// only its module directive, so a forwarded `--insecure-links` is inert.
    #[test]
    fn a_peer_supplied_optout_cannot_disarm_a_daemon() {
        let mut act = daemon(Role::Receiver, module_at("/srv/data"));
        act.insecure_links = LocalInsecureLinks::from_local_flag(true);
        assert!(!act.optout_allowed());
        assert!(act.hardened());
    }

    /// The module's own `insecure links = yes` does disarm it.
    #[test]
    fn a_module_directive_optout_disarms_a_daemon() {
        let mut module = module_at("/srv/data");
        module.insecure_links = ModuleInsecureLinks::from_module_config(true);
        let act = daemon(Role::Receiver, module);
        assert!(act.optout_allowed());
        assert!(!act.hardened());
    }

    /// Upstream gates the directive on `module_id >= 0`: before a module is
    /// selected there is no directive to honour, so the opt-out must not fire.
    #[test]
    fn an_unselected_module_cannot_opt_out() {
        let mut module = module_at("/srv/data");
        module.insecure_links = ModuleInsecureLinks::from_module_config(true);
        module.selected = false;
        assert!(!daemon(Role::Receiver, module).optout_allowed());
    }

    /// A daemon's boundary is its module root.
    #[test]
    fn a_daemon_root_is_the_module_root() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert_eq!(act.root(), Some(Path::new("/srv/data")));
    }

    /// `--confine-root` arrives in a peer-supplied argv, so obeying it inside a
    /// daemon could only loosen the module boundary.
    #[test]
    fn a_daemon_ignores_confine_root() {
        let mut act = daemon(Role::Receiver, module_at("/srv/data"));
        act.confine_root = Some(PathBuf::from("/tmp/anything"));
        assert_eq!(act.root(), Some(Path::new("/srv/data")));
    }

    /// The branch that actually matters: a daemon that has not resolved a
    /// module root must report *no* root rather than falling back to a
    /// peer-supplied `--confine-root`. Asserting only against a daemon that
    /// already has a module root cannot see this - the fallback never fires.
    #[test]
    fn a_daemon_without_a_module_root_does_not_fall_back_to_confine_root() {
        let mut act = daemon(Role::Receiver, ModuleState::default());
        act.confine_root = Some(PathBuf::from("/tmp/anything"));
        assert_eq!(act.root(), None);
    }

    /// A server launched by a restricted-shell wrapper gets its root from
    /// `--confine-root` instead.
    #[test]
    fn a_non_daemon_root_is_confine_root() {
        let mut act = not_daemon(Role::Receiver);
        act.confine_root = Some(PathBuf::from("/home/u/pub"));
        assert_eq!(act.root(), Some(Path::new("/home/u/pub")));
    }

    /// With no root at all nothing is outside anything, whatever the kind.
    #[test]
    fn nothing_is_outside_when_no_root_is_set() {
        let act = not_daemon(Role::Receiver);
        assert!(!act.outside_root(Path::new("/etc/shadow"), PathKind::Confined));
    }

    /// upstream `rootlen <= 1`: a module rooted at "/" confines nothing, and
    /// upstream returns 0 there *before* looking at the path at all. The
    /// relative case is what makes that early-out load-bearing - component
    /// comparison already handles every absolute path, so testing only
    /// absolute inputs cannot tell the guard from a no-op.
    #[test]
    fn a_root_of_slash_confines_nothing() {
        let act = daemon(Role::Receiver, module_at("/"));
        assert!(!act.outside_root(Path::new("/etc/shadow"), PathKind::Confined));
        assert!(!act.outside_root(Path::new("etc/shadow"), PathKind::Confined));
    }

    /// The whole point: a confined path that has diverged from the root is
    /// refused.
    #[test]
    fn a_confined_path_outside_the_root_is_refused() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert!(act.outside_root(Path::new("/etc/shadow"), PathKind::Confined));
    }

    /// `--log-file` and the `--*-from` family may legitimately live elsewhere,
    /// so the same path is accepted for an ancillary open.
    #[test]
    fn an_ancillary_path_outside_the_root_is_allowed() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert!(!act.outside_root(Path::new("/etc/shadow"), PathKind::Ancillary));
    }

    /// A path inside the root is fine, and so is the root itself.
    #[test]
    fn a_path_inside_the_root_is_allowed() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert!(!act.outside_root(Path::new("/srv/data/f"), PathKind::Confined));
        assert!(!act.outside_root(Path::new("/srv/data"), PathKind::Confined));
    }

    /// An absolute walk passes through the root's ancestors on the way down;
    /// those are not-yet-arrived, not outside.
    #[test]
    fn an_ancestor_of_the_root_is_still_descending() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert!(!act.outside_root(Path::new("/srv"), PathKind::Confined));
        assert!(!act.outside_root(Path::new("/"), PathKind::Confined));
    }

    /// The byte-prefix trap upstream guards with an explicit boundary test:
    /// `/srv/data-evil` shares a textual prefix with `/srv/data` but is a
    /// sibling, not a child. Component comparison makes this structural.
    #[test]
    fn a_sibling_sharing_a_textual_prefix_is_outside() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert!(act.outside_root(Path::new("/srv/data-evil/f"), PathKind::Confined));
    }

    /// An empty resolved path reads as an ancestor of everything, so it cannot
    /// be judged outside - upstream returns 0 for `alen == 0` for the same
    /// reason.
    #[test]
    fn an_empty_path_is_not_outside() {
        let act = daemon(Role::Receiver, module_at("/srv/data"));
        assert!(!act.outside_root(Path::new(""), PathKind::Confined));
    }
}
