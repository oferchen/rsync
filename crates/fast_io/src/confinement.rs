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
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The session's answer to [`Activation::optout_allowed`], published for the
/// ownership walk to read.
///
/// Upstream asks the question *inside* the walk - `ona_open()` opens with
/// `symlink_optout_allowed()` and returns a plain symlink-following `open()`
/// when it holds (`syscall.c:300-302`) - so the answer reaches both
/// `open_no_attacker_symlinks()` and `owner_walk_parent()` from one place. It
/// can do that because `insecure_links` and `am_daemon` are process globals.
///
/// Mirroring that shape is deliberate. The alternative - threading the flag
/// through every operator-path open - would touch ten call sites across seven
/// crates and every helper between them, and would let one un-threaded site
/// silently keep confining under an opt-out the operator asked for. The value
/// is a property of the session, not of the call, so it is stored once.
///
/// Only the derived bit is stored, never the flag itself: [`Activation`] stays
/// the single place the daemon-vs-local rule is written down, and this is the
/// answer it produced.
static SESSION_OPTOUT: AtomicBool = AtomicBool::new(false);

/// Publish `activation`'s opt-out answer for the ownership walk.
///
/// Call once, as early as the flag and the daemon/module state are both known.
/// Not calling it leaves the confinement fully engaged, which is the safe
/// default and upstream's own (`options.c:134` `int insecure_links = 0;`).
///
/// Most callers want [`install_local_session`] or [`install_daemon_session`]:
/// upstream's two arms read disjoint state, and those spell out which arm the
/// caller is without making it invent values for fields its arm ignores.
pub fn install_session(activation: &Activation) {
    SESSION_OPTOUT.store(activation.optout_allowed(), Ordering::Relaxed);
    // The pinned descriptor names the PREVIOUS root, so it stops being an
    // answer the moment the root changes. Dropping it here keeps one
    // invariant - the pin, when present, is always this root's - instead of
    // letting a stale descriptor answer for a root it never named.
    #[cfg(unix)]
    clear_session_root_fd();
    *SESSION_ROOT
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = activation.root().map(physical_root);
}

/// Resolve the confinement root to a PHYSICAL path.
///
/// This is load-bearing, not tidiness. The walk tracks where an open would
/// really land, after following every trusted symlink, so the path it offers
/// for judging is always physical. A root kept as the operator SPELLED it is
/// lexical, and comparing the two compares different namespaces: the first
/// version of this check over-refused two in-module controls on macOS, where
/// `/var` is a symlink to `/private/var`, so a root under `/var/folders/...`
/// never prefix-matched a walk that had already resolved to `/private/var/...`.
/// The same shape exists wherever an ancestor is a symlink - `/home ->
/// /usr/home` on FreeBSD, a bind-mounted or symlinked module root anywhere -
/// and it fails CLOSED, refusing paths that are plainly inside the module, so
/// it presents as a broken daemon rather than as a hole.
///
/// Resolving once, here, is also what keeps [`Activation`] free of I/O, which
/// is what lets its truth table compile and run on every target.
///
/// A root that cannot be resolved (it does not exist yet) keeps the name it was
/// given: that is the value the caller supplied, and substituting anything else
/// would be inventing a boundary.
fn physical_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// The session's answer to [`Activation::root`], published for the ownership
/// walk to read.
///
/// Stored for the same reason as [`SESSION_OPTOUT`]: upstream reads
/// `confinement_root()` from *inside* `ona_open()` because `module_dir` and
/// `confine_root` are process globals, so both entry points into the walk get
/// the same answer from one place. Threading the root through every operator
/// open instead would let one un-threaded site silently confine nothing, which
/// is the failure mode this exists to prevent. The value is a property of the
/// session, not of the call - unlike [`PathKind`], which is.
static SESSION_ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The session's root pinned *by identity* - the second half of
/// [`SESSION_ROOT`], not a second root.
///
/// [`SESSION_ROOT`] answers *where* the boundary is. This answers *which
/// directory* it is, as a descriptor obtained while the process could still
/// reach it. The two are always the same directory: [`install_session`] drops
/// the descriptor whenever it replaces the path, so a pin that is present is
/// always this session's.
///
/// # Why a descriptor is needed at all
///
/// A daemon resolves the module root while privileged and then drops to the
/// module's uid. Every later operation that re-walks the *absolute* module
/// path re-traverses the module's ancestors as the dropped uid, so a module
/// under a directory that uid cannot search (`path = /home/backup/data` with a
/// 0700 home) fails with `EACCES` even though the module itself is
/// world-readable. A descriptor opened before the drop is immune: the kernel
/// checks the ancestors once, at open time.
///
/// # Upstream Reference
///
/// - `clientserver.c:1059-1065` - `change_dir(module_chdir, CD_NORMAL)` then
///   `module_dirfd = open(".", O_RDONLY | O_DIRECTORY | O_CLOEXEC)`, both
///   above the `setgid`/`setuid` at `clientserver.c:1093+`.
/// - `flist.c:2035-2059` `secure_opendir()` - the scan anchors on it.
/// - `sender.c:293-295` - the content open anchors on it.
/// - `syscall.c:85-90` `open_anchor_dirfd()` - `dup(module_dirfd)` instead of
///   re-resolving the absolute root.
#[cfg(unix)]
static SESSION_ROOT_FD: RwLock<Option<PinnedRoot>> = RwLock::new(None);

/// The pinned root: the descriptor, plus the spelling it was pinned under.
///
/// Two spellings of one directory, not two roots. [`SESSION_ROOT`] holds the
/// *physical* one, because judging whether a resolved path landed outside the
/// boundary has to compare in one namespace. The sender's absolute source
/// paths are built from the *literal* one - the module path as the operator
/// wrote it in `rsyncd.conf` - and the two differ wherever an ancestor is a
/// symlink (`/var` -> `/private/var` on macOS, `/home` -> `/usr/home` on
/// FreeBSD, any bind-mounted or symlinked module root). Keeping the literal
/// here is what lets a lookup on such a deployment recognise its own root;
/// without it the anchoring silently never fires and the module is back to the
/// `EACCES` this exists to remove.
#[cfg(unix)]
#[derive(Clone)]
struct PinnedRoot {
    literal: PathBuf,
    fd: std::sync::Arc<std::os::fd::OwnedFd>,
}

/// Pin `root` by identity.
///
/// Call while the process can still reach it - for a daemon that means after
/// `chroot` and before the `setgid`/`setuid` drop, exactly where upstream
/// opens `module_dirfd`. Calling it later still succeeds when the ancestors
/// happen to be searchable, and fails with the same `EACCES` the pin exists to
/// avoid when they are not.
///
/// `root` is the root as the caller names it; it should be the same value the
/// caller published through [`install_session`], and both spellings of it are
/// then recognised (see [`PinnedRoot`]).
///
/// # Errors
///
/// The `open(2)` error from opening the root as a directory, or `ELOOP` when
/// the ownership walk refuses a component symlink owned by neither uid 0 nor
/// our euid. A caller that cannot pin must fall back to the absolute path -
/// which is what it did before a pin existed - and must not treat the pin as a
/// confinement it can skip re-checking.
#[cfg(unix)]
pub fn pin_session_root_fd(root: &Path) -> std::io::Result<()> {
    // upstream: clientserver.c:1065 - `open(".", O_RDONLY | O_DIRECTORY |
    // O_CLOEXEC)`. A working directory descriptor, not `O_PATH`: `openat`
    // anchoring, the `dup` in [`crate::open_trusted_dir`], and the Landlock
    // rule all take it as-is, and `O_PATH` cannot serve the first two.
    //
    // Through the OWNERSHIP WALK, not a plain open, because upstream's `.` is
    // already the directory `change_dir()` entered and `change_dir()` enters a
    // non-chrooted daemon's module root with
    // `open_no_attacker_symlinks(dir, O_RDONLY | O_DIRECTORY, 0)`
    // (`util1.c:1254-1263`). A plain open here would resolve a symlink an
    // attacker planted at a component of the configured `path =`, and the pin
    // would then answer `link_stat(".")` with the escape target's directory -
    // turning the ancestor-traversal fix into a module-root escape. The
    // operator's own `/backup -> /mnt/disk` is still followed; only a
    // foreign-owned one is refused.
    let fd = crate::owner_walk::operator_open_dir(root)?;
    *SESSION_ROOT_FD
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PinnedRoot {
        literal: root.to_path_buf(),
        fd: std::sync::Arc::new(fd),
    });
    Ok(())
}

/// Drop the pinned root descriptor.
#[cfg(unix)]
pub fn clear_session_root_fd() {
    *SESSION_ROOT_FD
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// The pinned root descriptor when `path` names the pinned root itself, under
/// either spelling of it.
///
/// This is the question [`crate::open_trusted_dir`] asks, and upstream asks it
/// as `strcmp(path, module_dir)` (`syscall.c:87`).
#[cfg(unix)]
#[must_use]
pub fn pinned_root_fd_for(path: &Path) -> Option<std::sync::Arc<std::os::fd::OwnedFd>> {
    let (fd, relative) = pinned_root_relative(path)?;
    (relative == Path::new(".")).then_some(fd)
}

/// Whether this session pinned a root at all.
#[cfg(unix)]
#[must_use]
pub fn session_root_is_pinned() -> bool {
    SESSION_ROOT_FD
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
}

/// Split `path` into the pinned root descriptor and the remainder to resolve
/// beneath it, so a caller can reach `path` without re-walking the root's
/// ancestors.
///
/// Returns `None` - meaning "resolve `path` the ordinary way" - when no root
/// is pinned, or when `path` does not lie beneath the pinned root. A path
/// equal to the root yields `.`, which is the name upstream's post-`change_dir`
/// code uses for the same directory (`flist.c:2059`).
#[cfg(unix)]
#[must_use]
pub fn pinned_root_relative(
    path: &Path,
) -> Option<(std::sync::Arc<std::os::fd::OwnedFd>, PathBuf)> {
    let pinned = SESSION_ROOT_FD
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()?;
    let relative = path
        .strip_prefix(&pinned.literal)
        .ok()
        .or_else(|| path.strip_prefix(session_confinement_root()?).ok())?;
    // A `..` in the remainder would be resolved from the pin rather than from
    // `/`, and `strip_prefix` is lexical so it cannot say where that lands.
    // Upstream refuses the same component up front
    // (`syscall.c` `path_has_dotdot_component`); here the answer is simply
    // "not anchorable", which leaves the caller on the absolute path it used
    // before. Anchoring is an optimisation of reach, never of policy, so
    // declining is always a correct answer.
    if relative
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return None;
    }
    let relative = if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative.to_path_buf()
    };
    Some((pinned.fd, relative))
}

/// Whether an already-resolved absolute path must be refused for landing
/// outside the session's confinement root.
///
/// The session-scoped counterpart of [`Activation::outside_root`], for the
/// ownership walk, which has no [`Activation`] in hand.
///
/// upstream: `syscall.c:186-240` `abspath_outside_confinement()`.
#[must_use]
pub fn outside_session_root(abspath: &Path, kind: PathKind) -> bool {
    SESSION_ROOT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_deref()
        .is_some_and(|root| path_outside_root(root, abspath, kind))
}

/// The session's confinement root, or `None` when nothing is confined.
///
/// The walk needs the value itself, not just the verdict: a *relative*
/// operator path has to be anchored somewhere before it can be judged.
///
/// upstream: `syscall.c:128-144` `confinement_root()`.
#[must_use]
pub fn session_confinement_root() -> Option<PathBuf> {
    SESSION_ROOT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The shared rule behind [`Activation::outside_root`] and
/// [`outside_session_root`]: one implementation, two ways of naming the root.
fn path_outside_root(root: &Path, abspath: &Path, kind: PathKind) -> bool {
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

/// Publish a non-daemon session: the local `--insecure-links` opt-out and the
/// `--confine-root` boundary.
///
/// The two arrive together because upstream's non-daemon arms read exactly
/// these two globals - `symlink_optout_allowed()` reads `insecure_links`,
/// `confinement_root()` reads `confine_root` - and a session that published one
/// without the other would answer half the question. Passing `None` for the
/// root is the ordinary case: without `--confine-root` a non-daemon transfer
/// confines nothing, exactly as upstream's NULL `confine_root` does.
///
/// # Upstream Reference
///
/// - `syscall.c:128-132` - the `am_daemon`-false arm of
///   `symlink_optout_allowed()` is `return insecure_links;`.
/// - `syscall.c:142-143` - the `am_daemon`-false arm of `confinement_root()`
///   returns `confine_root`.
pub fn install_local_session(insecure_links: LocalInsecureLinks, confine_root: Option<PathBuf>) {
    install_session(&Activation {
        // `daemon` is this arm's don't-care, spelled out here once rather than
        // at every call site.
        role: Role::Receiver,
        confine_root,
        daemon: DaemonState::NotDaemon,
        insecure_links,
    });
}

/// Publish the opt-out for a daemon serving `module`.
///
/// upstream: `syscall.c:125` - the `am_daemon`-true arm is
/// `module_id >= 0 && lp_insecure_links(module_id)`. A peer-supplied
/// `--insecure-links` is deliberately unreachable from here: a client cannot
/// switch off a daemon's confinement (`syscall.c:117-121`), which is why this
/// takes a [`ModuleState`] and not a [`LocalInsecureLinks`].
pub fn install_daemon_session(module: ModuleState) {
    install_session(&Activation {
        role: Role::Receiver,
        confine_root: None,
        daemon: DaemonState::Daemon(module),
        insecure_links: LocalInsecureLinks::default(),
    });
}

/// Whether this session opted out of the operator-path symlink confinement.
///
/// upstream: `syscall.c:301` - the `symlink_optout_allowed()` test at the top
/// of `ona_open()`.
#[must_use]
pub fn session_optout_allowed() -> bool {
    SESSION_OPTOUT.load(Ordering::Relaxed)
}

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
        self.root()
            .is_some_and(|root| path_outside_root(root, abspath, kind))
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
