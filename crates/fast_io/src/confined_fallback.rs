//! Upstream's three-arm fallback contract for a path-based syscall, in one
//! place.
//!
//! Every `do_*_at()` wrapper in upstream `syscall.c` answers the same question
//! before it touches the filesystem, and answers it the same way. Written out
//! from `do_unlink_at()` (`rsync-3.5.0/syscall.c:658`), which is the shortest
//! statement of it:
//!
//! ```c
//! if (operator_path_resolve) {
//!         if (symlink_optout_allowed())
//!                 return unlink(path);            /* arm 1 */
//!         dfd = owner_walk_parent(path, &bname);
//!         if (dfd < 0)
//!                 return -1;                      /* arm 3 */
//!         ret = unlinkat(dfd, bname, 0);          /* arm 2 */
//! ```
//!
//! | Arm | Condition | Action |
//! |---|---|---|
//! | 1 | the policy gate is off | a plain path syscall - deliberately unconfined, by STATIC POLICY |
//! | 2 | gate on, the parent walk SUCCEEDS | the `*at` syscall against `(parent dirfd, leaf)` |
//! | 3 | gate on, the parent walk FAILS | an error. NEVER a plain syscall |
//!
//! # Why this is a type and not five copies
//!
//! The three arms are easy to collapse into two - "try the confined form, and
//! on any error do the plain one" - and that collapse is the defect this module
//! exists to make unspellable. It turns arm 3 into arm 1, so a refusal the walk
//! issued *on purpose* (a foreign-owned parent symlink, a leaf outside the
//! confinement root) is laundered into the very syscall the refusal was
//! protecting against. Upstream names that test as wrong in its own words:
//! arm 1 is chosen from configuration read before any I/O happens, never from a
//! runtime errno.
//!
//! Keeping the decision in one type means a new operation inherits the contract
//! rather than restating it, and the arm a call took is observable
//! ([`ConfinedFallback::arm_for`]) so a test can tell arm 2 from arm 3 instead
//! of only seeing that something failed.
//!
//! # The gate, precisely
//!
//! Arm 1 is [`session_optout_allowed`](crate::confinement::session_optout_allowed) -
//! upstream `symlink_optout_allowed()` (`rsync-3.5.0/syscall.c:122`), which is
//! the whole gate on the operator arm because that arm is tested *above*
//! `secure_relpath_active()` in every wrapper.
//!
//! `secure_relpath_active()` (`rsync-3.5.0/syscall.c:100`) is not a second gate
//! here. Its first clause IS the opt-out, so it can never admit a call this
//! module refuses; its remaining clauses (chroot, sender) select upstream's
//! *other* tier, which resolves a transfer-relative parent through
//! `secure_relative_open()` rather than through the ownership walk. That is a
//! different resolver, not a different arm of this one.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:658` `do_unlink_at()` - the contract, quoted above.
//! - `rsync-3.5.0/syscall.c:1402` `do_rmdir_at()` - same shape, `AT_REMOVEDIR`.
//! - `rsync-3.5.0/syscall.c:1497` `do_open_at()` - same shape, and the leaf
//!   carries `O_NOFOLLOW`.
//! - `rsync-3.5.0/syscall.c:1866` `do_rename_at()` - same shape, both endpoints
//!   walked independently.
//! - `rsync-3.5.0/syscall.c:558` `owner_walk_parent()` - the walk arm 2 and
//!   arm 3 share, mirrored by [`owner_trusted_parent_kind`].

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use crate::confinement::PathKind;
use crate::dir_sandbox::{UnlinkFlags, UnlinkResidue};
use crate::owner_walk::owner_trusted_parent_kind;

/// Which arm a resolution took, for callers and tests that need to see the
/// decision rather than only its result.
///
/// Arm 3 is not a variant: it is the `Err` of the `Result` that carries this.
/// Spelling it as a third variant would let a caller ignore a refusal by
/// matching on it, which is the collapse this module prevents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackArm {
    /// Arm 1 - the policy gate is off, so the operation runs as a plain path
    /// syscall. Unconfined by static policy, not by accident.
    Unconfined,
    /// Arm 2 - the gate is on and the parent walk succeeded, so the operation
    /// runs `*at` against the walked parent.
    Confined,
}

/// The resolved parent for arm 2, or the decision that arm 1 applies.
enum Resolved {
    Unconfined,
    Confined { parent: OwnedFd, leaf: OsString },
}

/// The single owner of upstream's three-arm fallback contract.
///
/// Construct one per call site with the [`PathKind`] that site's path has -
/// upstream toggles `operator_path_resolve` around each site for the same
/// reason - then issue operations against it.
///
/// # Examples
///
/// ```no_run
/// use fast_io::ConfinedFallback;
/// use std::path::Path;
///
/// // A peer-named destination entry: confined to the session root.
/// ConfinedFallback::confined().rmdir_at(Path::new("dest/stale"))?;
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfinedFallback {
    kind: PathKind,
}

impl ConfinedFallback {
    /// A path that must stay inside the session's confinement root.
    ///
    /// upstream: `operator_path_resolve = 1` around the call site.
    #[must_use]
    pub const fn confined() -> Self {
        Self {
            kind: PathKind::Confined,
        }
    }

    /// A path that may legitimately live outside the tree - still walked for
    /// foreign-owned symlinks, but not judged against the root.
    ///
    /// upstream: `operator_path_resolve = 0` around the call site.
    #[must_use]
    pub const fn ancillary() -> Self {
        Self {
            kind: PathKind::Ancillary,
        }
    }

    /// Which arm `path` resolves to, without performing any operation.
    ///
    /// `Ok(Unconfined)` is arm 1, `Ok(Confined)` is arm 2, and `Err` is arm 3.
    /// Exposed so a test can discriminate arm 2 from arm 3: both leave the
    /// filesystem in a state a plain "did it fail?" assertion cannot tell
    /// apart from a merely absent file.
    ///
    /// # Errors
    ///
    /// Arm 3: whatever the ownership walk refused with - `ELOOP` for a
    /// foreign-owned symlink component or a leaf outside the confinement root,
    /// `EINVAL` for a path with no final component, or the underlying
    /// `openat`/`statat` errno.
    pub fn arm_for(&self, path: &Path) -> io::Result<FallbackArm> {
        match self.resolve(path)? {
            Resolved::Unconfined => Ok(FallbackArm::Unconfined),
            Resolved::Confined { .. } => Ok(FallbackArm::Confined),
        }
    }

    /// Remove a non-directory entry at `path`.
    ///
    /// `entry` selects `unlinkat(2)`'s flag word exactly as upstream does:
    /// [`UnlinkFlags::File`] is `flags == 0` (`do_unlink_at`) and
    /// [`UnlinkFlags::Dir`] is `AT_REMOVEDIR` (`do_rmdir_at`). [`rmdir_at`] is
    /// the named spelling of the second.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:674` - arm 1, `return unlink(path)`.
    /// - `rsync-3.5.0/syscall.c:676` - the walk shared by arms 2 and 3.
    /// - `rsync-3.5.0/syscall.c:680` - arm 2, `unlinkat(dfd, bname, 0)`.
    ///
    /// [`rmdir_at`]: Self::rmdir_at
    ///
    /// # Errors
    ///
    /// Arm 3's refusal, or the `unlink(2)` / `unlinkat(2)` errno.
    pub fn unlink_at(&self, path: &Path, entry: UnlinkFlags) -> io::Result<()> {
        match self.resolve(path)? {
            Resolved::Unconfined => crate::unlink_path(path, entry),
            Resolved::Confined { parent, leaf } => {
                crate::unlinkat(parent.as_fd(), leaf.as_os_str(), entry)
            }
        }
    }

    /// Remove the empty directory at `path`.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:1402` `do_rmdir_at()` - the same three
    /// arms with `AT_REMOVEDIR` set "to require the target be a directory".
    ///
    /// # Errors
    ///
    /// Arm 3's refusal, or the `rmdir(2)` / `unlinkat(2)` errno - notably
    /// `ENOTEMPTY` when the directory still holds entries.
    pub fn rmdir_at(&self, path: &Path) -> io::Result<()> {
        self.unlink_at(path, UnlinkFlags::Dir)
    }

    /// Remove the directory at `path` and everything beneath it.
    ///
    /// Upstream has no `do_remove_dir_all_at()`: its recursive delete is the
    /// receiver's own peel, built from the single-entry wrappers. The contract
    /// is therefore applied where upstream applies it - to the *entry point*,
    /// so the descent starts from a walked parent rather than from a path the
    /// kernel re-resolves. The peel itself is [`recursive_unlinkat`], which is
    /// already dirfd-anchored the whole way down.
    ///
    /// [`recursive_unlinkat`]: crate::recursive_unlinkat
    ///
    /// # Errors
    ///
    /// Arm 3's refusal, or the descent's error. On arm 1 the residue reports a
    /// clean removal, because [`std::fs::remove_dir_all`] returns `Err` rather
    /// than surviving entries.
    pub fn remove_dir_all_at(&self, path: &Path) -> io::Result<UnlinkResidue> {
        match self.resolve(path)? {
            Resolved::Unconfined => std::fs::remove_dir_all(path).map(|()| UnlinkResidue {
                not_empty: false,
                had_errors: false,
            }),
            Resolved::Confined { parent, leaf } => {
                crate::recursive_unlinkat(parent.as_fd(), leaf.as_os_str())
            }
        }
    }

    /// Rename `old_path` to `new_path`.
    ///
    /// Each endpoint is resolved independently, mirroring upstream: an absolute
    /// source must not be able to switch off confinement of a relative
    /// destination. Both sides take the same arm, because the arm is a property
    /// of the session rather than of either path.
    ///
    /// `replace` false selects `RENAME_NOREPLACE` where the platform has it;
    /// upstream's `do_rename_at()` is the `replace == true` shape.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:1892` - arm 1, `return do_rename(...)`.
    /// - `rsync-3.5.0/syscall.c:1894` - the source-side walk.
    /// - `rsync-3.5.0/syscall.c:1904` - arm 2, `renameat` between both dirfds.
    ///
    /// # Errors
    ///
    /// Arm 3's refusal from either side, or the `rename(2)` / `renameat(2)`
    /// errno.
    pub fn rename_at(&self, old_path: &Path, new_path: &Path, replace: bool) -> io::Result<()> {
        match (self.resolve(old_path)?, self.resolve(new_path)?) {
            (
                Resolved::Confined {
                    parent: old_dir,
                    leaf: old_leaf,
                },
                Resolved::Confined {
                    parent: new_dir,
                    leaf: new_leaf,
                },
            ) => crate::renameat(
                old_dir.as_fd(),
                old_leaf.as_os_str(),
                new_dir.as_fd(),
                new_leaf.as_os_str(),
                replace,
            ),
            // Arm 1 on both sides: `resolve` reads one session-scoped answer,
            // so a mixed pair cannot occur. Spelling the arm once keeps that a
            // fact about `resolve` rather than a rule repeated here.
            _ => crate::renameat(
                rustix::fs::CWD,
                old_path.as_os_str(),
                rustix::fs::CWD,
                new_path.as_os_str(),
                replace,
            ),
        }
    }

    /// Open `path`.
    ///
    /// On arm 2 the leaf carries `O_NOFOLLOW` in addition to `flags`, exactly
    /// as upstream does - "so the basename itself isn't followed if it happens
    /// to be a pre-planted symlink, which is what we want for `O_CREAT|O_EXCL`"
    /// (`rsync-3.5.0/syscall.c:1493`). The walk defends the parent chain; the
    /// leaf is a separate decision and needs its own flag.
    ///
    /// On arm 1 the flags are passed verbatim: upstream's arm 1 is
    /// `do_open(pathname, flags, mode)`, the legacy symlink-following open, and
    /// adding `O_NOFOLLOW` there would make the opt-out stricter than the
    /// behaviour it exists to restore.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/syscall.c:1514` - arm 1, `return do_open(...)`.
    /// - `rsync-3.5.0/syscall.c:1516` - the walk shared by arms 2 and 3.
    /// - `rsync-3.5.0/syscall.c:1518` - arm 2,
    ///   `openat(dfd, bname, flags | O_NOFOLLOW, mode)`.
    ///
    /// # Errors
    ///
    /// Arm 3's refusal, or the `open(2)` / `openat(2)` errno - notably `ELOOP`
    /// on arm 2 when the leaf itself is a symlink.
    pub fn open_at(&self, path: &Path, flags: i32, mode: u32) -> io::Result<File> {
        match self.resolve(path)? {
            Resolved::Unconfined => crate::openat(rustix::fs::CWD, path.as_os_str(), flags, mode),
            Resolved::Confined { parent, leaf } => crate::openat(
                parent.as_fd(),
                leaf.as_os_str(),
                flags | libc::O_NOFOLLOW,
                mode,
            ),
        }
    }

    /// The one place the three arms are decided.
    ///
    /// Arm 1 is read from session policy before any I/O; arm 2 and arm 3 are
    /// the two outcomes of the ownership walk. There is deliberately no path
    /// from an `Err` here back to [`Resolved::Unconfined`].
    ///
    /// upstream: `rsync-3.5.0/syscall.c:674-679` - the `symlink_optout_allowed()`
    /// test, then `owner_walk_parent()`, then `if (dfd < 0) return -1;`.
    ///
    /// # Measured: the gate is only PARTLY redundant
    ///
    /// `owner_walk_open` short-circuits on the same opt-out
    /// (`owner_walk.rs`, mirroring `syscall.c:300-302`), so for the `*at` ops
    /// the two arms resolve the same parent either way. Removing this test
    /// therefore leaves the delete and rename cells green - measured, by
    /// mutation. It is NOT redundant for the two decisions the walk cannot
    /// make: which arm [`ConfinedFallback::arm_for`] reports, and whether
    /// [`ConfinedFallback::open_at`] adds `O_NOFOLLOW` to the leaf. Both are
    /// pinned, and both go red without it.
    fn resolve(&self, path: &Path) -> io::Result<Resolved> {
        if crate::confinement::session_optout_allowed() {
            return Ok(Resolved::Unconfined);
        }
        let (parent, leaf) = owner_trusted_parent_kind(path, self.kind)?;
        Ok(Resolved::Confined { parent, leaf })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confinement::{Activation, DaemonState, LocalInsecureLinks, Role, install_session};
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// The confinement session is process-global (upstream reads the same
    /// answer from globals), so each cell below installs the policy it needs.
    /// nextest runs one process per test, which is what keeps that sound; these
    /// cells are not safe under a shared-process runner.
    fn confine_to(root: &Path) {
        install_session(&Activation {
            role: Role::Receiver,
            daemon: DaemonState::NotDaemon,
            insecure_links: LocalInsecureLinks::default(),
            confine_root: Some(root.to_path_buf()),
        });
    }

    /// The same session WITH the opt-out set: the arm-1 counterpart of
    /// [`confine_to`].
    ///
    /// Keeping the root installed is what makes an arm-1 cell a ONE-VARIABLE
    /// flip of its arm-3 twin. Dropping the root as well would leave two
    /// differences, and the cell would then also pass with the gate removed -
    /// the walk would simply have nothing to judge the leaf against.
    fn confine_to_with_optout(root: &Path) {
        install_session(&Activation {
            role: Role::Receiver,
            daemon: DaemonState::NotDaemon,
            insecure_links: LocalInsecureLinks::from_local_flag(true),
            confine_root: Some(root.to_path_buf()),
        });
    }

    /// A tree whose only difference between arm 2 and arm 3 is WHERE the path
    /// lands, so the two are told apart by the confinement decision and not by
    /// an incidental failure:
    ///
    /// ```text
    /// temp/module/          <- confinement root
    /// temp/module/payload   <- in-root leaf     (arm 2)
    /// temp/module/esc       -> ../outside       (our own symlink: FOLLOWED)
    /// temp/outside/secret   <- out-of-root leaf (arm 3, reached via esc)
    /// temp/outside/subdir/  <- out-of-root tree (arm 3, recursive)
    /// ```
    struct Fixture {
        _temp: TempDir,
        root: PathBuf,
        /// `module/payload` - inside the root.
        inside: PathBuf,
        /// `module/esc/secret` - spelled inside the root, lands outside it.
        escape: PathBuf,
        /// `module/esc/subdir` - the recursive shape of `escape`.
        escape_dir: PathBuf,
        /// The real out-of-root file `escape` resolves to.
        victim: PathBuf,
        /// The real out-of-root directory `escape_dir` resolves to.
        victim_dir: PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        let inside = root.join("payload");
        std::fs::write(&inside, b"INSIDE").expect("write inside");

        let outside_dir = temp.path().join("outside");
        std::fs::create_dir(&outside_dir).expect("mkdir outside");
        let victim = outside_dir.join("secret");
        std::fs::write(&victim, b"OUTSIDE").expect("write outside");
        let victim_dir = outside_dir.join("subdir");
        std::fs::create_dir(&victim_dir).expect("mkdir subdir");
        std::fs::write(victim_dir.join("child"), b"CHILD").expect("write child");

        symlink("../outside", root.join("esc")).expect("symlink esc");

        Fixture {
            _temp: temp,
            escape: root.join("esc").join("secret"),
            escape_dir: root.join("esc").join("subdir"),
            root,
            inside,
            victim,
            victim_dir,
        }
    }

    // --- arm classification -------------------------------------------------

    /// Arm 1: the opt-out is a STATIC POLICY answer, read before any I/O, so a
    /// path that arm 3 refuses classifies as unconfined here.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:674` - `symlink_optout_allowed()` is
    /// tested at the top of the operator arm, above the walk.
    #[test]
    fn arm_one_is_chosen_when_the_session_opted_out() {
        let fx = fixture();
        confine_to_with_optout(&fx.root);
        assert_eq!(
            ConfinedFallback::confined()
                .arm_for(&fx.escape)
                .expect("the opt-out short-circuits before the walk"),
            FallbackArm::Unconfined
        );
    }

    /// Arm 2: the gate is on and the walk reaches an in-root parent.
    ///
    /// This is the control for every arm-3 cell below. Without it those cells
    /// would also pass if the walk refused everything, which is the
    /// availability failure this contract must not introduce.
    #[test]
    fn arm_two_is_chosen_when_the_walk_reaches_an_in_root_parent() {
        let fx = fixture();
        confine_to(&fx.root);
        assert_eq!(
            ConfinedFallback::confined()
                .arm_for(&fx.inside)
                .expect("an in-root parent resolves"),
            FallbackArm::Confined
        );
    }

    /// Arm 3: the gate is on and the walk refuses, so the result is an error -
    /// NOT arm 1. `ELOOP` identifies it as the confinement decision rather than
    /// an incidental traversal failure.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:677-678` - `if (dfd < 0) return -1;`.
    #[test]
    fn arm_three_is_an_error_and_never_falls_back_to_arm_one() {
        let fx = fixture();
        confine_to(&fx.root);
        let error = ConfinedFallback::confined()
            .arm_for(&fx.escape)
            .expect_err("a parent that lands outside the root must be refused");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ELOOP),
            "the refusal must be the confinement decision"
        );
    }

    /// The same escaping path under `Ancillary` is arm 2, not arm 3: only a
    /// confined path is judged against the root.
    ///
    /// upstream: `operator_path_resolve` is per call site, and
    /// `abspath_outside_confinement()` returns 0 when it is clear
    /// (`rsync-3.5.0/syscall.c:216`).
    #[test]
    fn an_ancillary_path_is_not_judged_against_the_root() {
        let fx = fixture();
        confine_to(&fx.root);
        assert_eq!(
            ConfinedFallback::ancillary()
                .arm_for(&fx.escape)
                .expect("an ancillary path may live outside the tree"),
            FallbackArm::Confined
        );
    }

    // --- unlink -------------------------------------------------------------

    /// Arm 3 must leave the out-of-root file ALIVE. This is the whole point of
    /// the contract: an "on any error, do the plain syscall" collapse deletes
    /// it, because the refusal is exactly what the plain syscall ignores.
    #[test]
    fn unlink_arm_three_refuses_and_the_out_of_root_file_survives() {
        let fx = fixture();
        confine_to(&fx.root);
        let error = ConfinedFallback::confined()
            .unlink_at(&fx.escape, UnlinkFlags::File)
            .expect_err("an escaping unlink must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        assert!(
            fx.victim.exists(),
            "the refusal must not be laundered into a plain unlink"
        );
    }

    /// The non-vacuity companion: the SAME call on the SAME path, differing
    /// only in policy, really does remove the file on arm 1. Without this the
    /// cell above would also pass if `unlink_at` could never delete anything.
    #[test]
    fn unlink_arm_one_performs_the_plain_syscall() {
        let fx = fixture();
        confine_to_with_optout(&fx.root);
        ConfinedFallback::confined()
            .unlink_at(&fx.escape, UnlinkFlags::File)
            .expect("the opt-out restores the legacy symlink-following unlink");
        assert!(
            !fx.victim.exists(),
            "arm 1 is the plain path syscall, which follows the parent symlink"
        );
    }

    /// Arm 2 still removes an in-root entry - the availability half.
    #[test]
    fn unlink_arm_two_removes_an_in_root_entry() {
        let fx = fixture();
        confine_to(&fx.root);
        ConfinedFallback::confined()
            .unlink_at(&fx.inside, UnlinkFlags::File)
            .expect("an in-root unlink still works");
        assert!(!fx.inside.exists());
    }

    // --- rmdir --------------------------------------------------------------

    #[test]
    fn rmdir_arm_three_refuses_and_the_out_of_root_directory_survives() {
        let fx = fixture();
        std::fs::remove_file(fx.victim_dir.join("child")).expect("empty the victim dir");
        confine_to(&fx.root);
        let error = ConfinedFallback::confined()
            .rmdir_at(&fx.escape_dir)
            .expect_err("an escaping rmdir must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        assert!(fx.victim_dir.exists());
    }

    #[test]
    fn rmdir_arm_two_removes_an_in_root_directory() {
        let fx = fixture();
        let doomed = fx.root.join("empty");
        std::fs::create_dir(&doomed).expect("mkdir empty");
        confine_to(&fx.root);
        ConfinedFallback::confined()
            .rmdir_at(&doomed)
            .expect("an in-root rmdir still works");
        assert!(!doomed.exists());
    }

    // --- recursive remove ---------------------------------------------------

    #[test]
    fn remove_dir_all_arm_three_refuses_and_the_out_of_root_tree_survives() {
        let fx = fixture();
        confine_to(&fx.root);
        let error = ConfinedFallback::confined()
            .remove_dir_all_at(&fx.escape_dir)
            .expect_err("an escaping recursive delete must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        assert!(
            fx.victim_dir.join("child").exists(),
            "the descent must not start from a path the kernel re-resolves"
        );
    }

    #[test]
    fn remove_dir_all_arm_two_peels_an_in_root_tree() {
        let fx = fixture();
        let doomed = fx.root.join("tree");
        std::fs::create_dir(&doomed).expect("mkdir tree");
        std::fs::write(doomed.join("child"), b"x").expect("write child");
        confine_to(&fx.root);
        let residue = ConfinedFallback::confined()
            .remove_dir_all_at(&doomed)
            .expect("an in-root recursive delete still works");
        assert!(!residue.not_empty && !residue.had_errors);
        assert!(!doomed.exists());
    }

    // --- rename -------------------------------------------------------------

    /// A refused DESTINATION must refuse the whole rename: upstream walks each
    /// endpoint independently and returns -1 the moment either side fails, so
    /// an in-root source cannot license an escaping target.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:1896-1903`.
    #[test]
    fn rename_arm_three_refuses_when_only_the_destination_escapes() {
        let fx = fixture();
        confine_to(&fx.root);
        let target = fx.root.join("esc").join("planted");
        let error = ConfinedFallback::confined()
            .rename_at(&fx.inside, &target, true)
            .expect_err("an escaping destination must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        assert!(fx.inside.exists(), "the source must be untouched");
        assert!(
            !fx.victim.with_file_name("planted").exists(),
            "nothing may be published outside the root"
        );
    }

    #[test]
    fn rename_arm_two_renames_within_the_root() {
        let fx = fixture();
        confine_to(&fx.root);
        let target = fx.root.join("renamed");
        ConfinedFallback::confined()
            .rename_at(&fx.inside, &target, true)
            .expect("an in-root rename still works");
        assert!(target.exists() && !fx.inside.exists());
    }

    // --- open ---------------------------------------------------------------

    #[test]
    fn open_arm_three_refuses_an_escaping_parent() {
        let fx = fixture();
        confine_to(&fx.root);
        let error = ConfinedFallback::confined()
            .open_at(&fx.escape, libc::O_RDONLY, 0)
            .expect_err("an escaping open must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
    }

    /// Arm 2 applies `O_NOFOLLOW` to the LEAF, which the flag-translating
    /// fallback in `dir_sandbox::at_syscalls::open` silently drops. The link
    /// here points at an in-root file, so nothing but the missing flag can
    /// refuse it.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:1518` -
    /// `openat(dfd, bname, flags | O_NOFOLLOW, mode)`.
    #[test]
    fn open_arm_two_applies_o_nofollow_to_the_leaf() {
        let fx = fixture();
        let leaf_link = fx.root.join("leaf");
        symlink(&fx.inside, &leaf_link).expect("symlink leaf");
        confine_to(&fx.root);
        let error = ConfinedFallback::confined()
            .open_at(&leaf_link, libc::O_RDONLY, 0)
            .expect_err("a leaf symlink must not be followed on arm 2");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ELOOP),
            "O_NOFOLLOW must reach the leaf"
        );
    }

    /// The mirror half, and the reason arm 1 does NOT add `O_NOFOLLOW`: the
    /// opt-out restores the legacy symlink-following open verbatim, so the same
    /// leaf link opens.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:1514` - arm 1 is `do_open(pathname,
    /// flags, mode)`, with no added flag.
    #[test]
    fn open_arm_one_still_follows_a_leaf_symlink() {
        let fx = fixture();
        let leaf_link = fx.root.join("leaf");
        symlink(&fx.inside, &leaf_link).expect("symlink leaf");
        confine_to_with_optout(&fx.root);
        let mut opened = ConfinedFallback::confined()
            .open_at(&leaf_link, libc::O_RDONLY, 0)
            .expect("arm 1 keeps the legacy following open");
        let mut body = String::new();
        std::io::Read::read_to_string(&mut opened, &mut body).expect("read");
        assert_eq!(body, "INSIDE");
    }

    #[test]
    fn open_arm_two_opens_an_in_root_leaf() {
        let fx = fixture();
        confine_to(&fx.root);
        let mut opened = ConfinedFallback::confined()
            .open_at(&fx.inside, libc::O_RDONLY, 0)
            .expect("an in-root open still works");
        let mut body = String::new();
        std::io::Read::read_to_string(&mut opened, &mut body).expect("read");
        assert_eq!(body, "INSIDE");
    }
}
