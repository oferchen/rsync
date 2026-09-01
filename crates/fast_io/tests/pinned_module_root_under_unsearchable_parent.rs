//! A daemon must serve a module whose ancestors it can no longer traverse.
//!
//! Upstream never re-walks the absolute module path after the privilege drop:
//! `clientserver.c:1059-1065` `change_dir()`s into the module root and pins it
//! as `module_dirfd` while still privileged, and the scan
//! (`flist.c:2035-2059`), the content open (`sender.c:293-295`) and the anchor
//! helper (`syscall.c:85-90`, `dup(module_dirfd)`) all resolve against that
//! descriptor afterwards. oc keeps absolute source paths, so without the same
//! pin every one of those lookups re-traverses the module's ancestors under
//! the dropped identity and fails with `EACCES` on an unsearchable one - the
//! `path = /home/backup/data` under a 0700 home shape.
//!
//! # How the barrier is built without root
//!
//! The kernel rule is search permission on an ancestor; `uid` is only how a
//! daemon usually loses it. `chmod 0` on a parent takes it away from the
//! current user just as effectively, and root is exempt from both - so the
//! cell asserts its own barrier first and skips as root, where no barrier can
//! be built.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fast_io::confinement::{
    ModuleInsecureLinks, ModuleState, clear_session_root_fd, install_daemon_session,
    pin_session_root_fd, session_root_is_pinned,
};

/// The fixture: `<tmp>/private/mod/{sub/file, link}`, plus `<tmp>/outside`.
struct Fixture {
    _temp: tempfile::TempDir,
    private: PathBuf,
    module: PathBuf,
}

impl Fixture {
    fn build() -> io::Result<Self> {
        let temp = tempfile::tempdir()?;
        let private = temp.path().join("private");
        let module = private.join("mod");
        let sub = module.join("sub");
        fs::create_dir_all(&sub)?;
        fs::write(sub.join("file"), b"served\n")?;
        std::os::unix::fs::symlink("sub/file", module.join("link"))?;
        fs::set_permissions(&module, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755))?;
        Ok(Self {
            _temp: temp,
            private,
            module,
        })
    }

    /// Take away search permission on the module's parent.
    fn seal(&self) -> io::Result<()> {
        fs::set_permissions(&self.private, fs::Permissions::from_mode(0o000))
    }

    /// Give it back, so the fixture can be torn down.
    fn unseal(&self) -> io::Result<()> {
        fs::set_permissions(&self.private, fs::Permissions::from_mode(0o755))
    }

    /// Publish the module as the session root and pin it by identity, the way
    /// the daemon does between its chroot and its `setuid`.
    fn publish_and_pin(&self) -> io::Result<()> {
        install_daemon_session(ModuleState {
            root: Some(self.module.clone()),
            chrooted: false,
            selected: true,
            insecure_links: ModuleInsecureLinks::default(),
        });
        pin_session_root_fd(&self.module)
    }
}

/// Sorted child names, so the two arms are compared as sets and not as
/// whatever order the filesystem happened to return.
fn child_names(entries: fast_io::pinned_root::ReadDir) -> io::Result<Vec<OsString>> {
    let mut names = entries
        .map(|entry| entry.map(|path| path.file_name().unwrap_or_default().to_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

fn eacces(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EACCES)
}

/// Everything the sender does to a module path, with the module's parent
/// sealed shut and the root pinned beforehand.
///
/// One test function on purpose: the pin and the session root are process
/// globals (as they are upstream), so these assertions are an ordered sequence
/// against one installed session, not independent cells that could interleave.
#[test]
fn a_pinned_module_root_survives_an_unsearchable_parent() {
    if unsafe_geteuid() == 0 {
        // Root is exempt from the search-permission check, so no barrier can
        // be built here and every arm below would pass without proving
        // anything. The equivalent root-only cell is upstream's own
        // `testsuite/daemon-module-private-parent_test.py`.
        return;
    }

    let fixture = Fixture::build().expect("fixture");
    let module = fixture.module.clone();
    let sub_file = module.join("sub").join("file");

    // ---- Non-vacuity: an ordinary, traversable parent still works, with and
    // ---- without a pin. A fix that broke the ordinary path would sail past
    // ---- every assertion below it.
    clear_session_root_fd();
    let unpinned_meta = fast_io::pinned_root::symlink_metadata(&module).expect("unpinned lstat");
    assert!(unpinned_meta.is_dir(), "the module root is a directory");
    let unpinned_names =
        child_names(fast_io::pinned_root::read_dir(&module).expect("unpinned scan"))
            .expect("unpinned entries");
    assert_eq!(
        unpinned_names,
        vec![OsString::from("link"), OsString::from("sub")],
        "the unpinned scan must list the module's children"
    );

    fixture.publish_and_pin().expect("publish and pin");
    assert!(
        session_root_is_pinned(),
        "publishing a module root and pinning it must leave a descriptor behind"
    );
    let pinned_names = child_names(fast_io::pinned_root::read_dir(&module).expect("pinned scan"))
        .expect("pinned entries");
    assert_eq!(
        pinned_names, unpinned_names,
        "the pinned scan must see exactly what the unpinned one saw"
    );

    // ---- The barrier goes up. Everything from here on runs with the module's
    // ---- parent unsearchable, which is the state a daemon reaches by
    // ---- dropping to the module uid.
    fixture.seal().expect("seal the parent");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sealed_assertions(&module, &sub_file);
    }));
    fixture.unseal().expect("unseal the parent");
    clear_session_root_fd();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }

    an_operator_owned_symlink_at_the_module_root_is_followed_and_pinned();
}

fn sealed_assertions(module: &Path, sub_file: &Path) {
    // The barrier is real: assert it before relying on it. Without this the
    // whole cell could pass on a filesystem or platform where `chmod 0` on a
    // directory does not stop a descendant lookup, and would be proving
    // nothing at all.
    let control = fs::symlink_metadata(module).expect_err("a sealed parent must block lstat");
    assert!(
        eacces(&control),
        "expected EACCES through the sealed parent, got {control}"
    );
    let control = fs::read_dir(module).expect_err("a sealed parent must block opendir");
    assert!(
        eacces(&control),
        "expected EACCES through the sealed parent, got {control}"
    );

    // Consumer 1: the walk's `link_stat`. This is the exact lookup that
    // produced `link_stat "." (in m) failed: Permission denied`.
    sealed_stat_assertions(module, sub_file);

    // Consumer 2: the walk's `read_dir` and the per-child stat it drives.
    let names = child_names(
        fast_io::pinned_root::read_dir(module)
            .expect("the pinned scan must not re-walk the sealed parent"),
    )
    .expect("pinned entries");
    assert_eq!(names, vec![OsString::from("link"), OsString::from("sub")]);

    // Consumer 3: the anchor every confined source open resolves against.
    // `open_trusted_dir` is oc's `open_anchor_dirfd()` (syscall.c:85-90), and
    // it is what `SourceOpen::open` and `read_source_link` re-open on every
    // call.
    fast_io::open_trusted_dir(module)
        .expect("the trusted anchor open must duplicate the pin, not re-resolve the path");
    let mut file = fast_io::open_source_confined(
        module,
        Path::new("sub/file"),
        fast_io::LeafPolicy::Nofollow,
        false,
    )
    .expect("the confined content open must resolve against the pin");
    let mut body = String::new();
    io::Read::read_to_string(&mut file, &mut body).expect("read the served file");
    assert_eq!(body, "served\n");
    let followed = fast_io::open_source_confined(
        module,
        Path::new("link"),
        fast_io::LeafPolicy::FollowConfined,
        false,
    );
    assert!(
        followed.is_ok(),
        "the --copy-links content open must resolve against the pin too: {followed:?}"
    );
    let target = fast_io::read_link_confined(module, Path::new("link"))
        .expect("the confined readlink must resolve against the pin");
    assert_eq!(target, Path::new("sub/file"));

    // The mutation, in-process: with the pin dropped, every one of the above
    // is back to re-walking the absolute module path, and the sealed parent
    // stops it. Without this the cell could pass because the sealed parent
    // never blocked anything.
    clear_session_root_fd();
    let regressed = fast_io::open_trusted_dir(module)
        .expect_err("without the pin the anchor open must re-resolve the path and fail");
    assert!(
        eacces(&regressed),
        "expected EACCES without the pin, got {regressed}"
    );
    let regressed = fast_io::pinned_root::read_dir(module)
        .err()
        .expect("without the pin the scan must re-walk the sealed parent and fail");
    assert!(
        eacces(&regressed),
        "expected EACCES without the pin, got {regressed}"
    );
    sealed_stat_mutation(module);
}

/// The stat half of the anchoring, which exists only where `O_PATH` does.
///
/// `O_PATH` is the only open that reports a directory entry's metadata without
/// requiring access to it and without following a symlinked leaf, so the
/// anchored stat is Linux-only by construction; elsewhere it is the ordinary
/// path-based `lstat`/`stat` it has always been, and the sealed parent stops
/// it exactly as it always did. See `fast_io::pinned_root`'s Platform section.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn sealed_stat_assertions(module: &Path, sub_file: &Path) {
    let meta = fast_io::pinned_root::symlink_metadata(module)
        .expect("the pinned lstat of the module root must not re-walk the sealed parent");
    assert!(meta.is_dir(), "the module root is still a directory");
    let meta = fast_io::pinned_root::metadata(sub_file)
        .expect("the pinned stat of an in-module file must not re-walk the sealed parent");
    assert_eq!(meta.len(), b"served\n".len() as u64);

    // A symlink must be lstat'ed as a symlink and stat'ed as its target: the
    // anchored arm must not collapse the two, which is the one way an
    // `O_PATH`-based stat could silently change meaning.
    let link = module.join("link");
    assert!(
        fast_io::pinned_root::symlink_metadata(&link)
            .expect("pinned lstat of the symlink")
            .file_type()
            .is_symlink(),
        "the pinned lstat must report the symlink itself"
    );
    assert!(
        fast_io::pinned_root::metadata(&link)
            .expect("pinned stat of the symlink")
            .is_file(),
        "the pinned stat must follow the symlink to its target"
    );
}

/// The stat half of the mutation. Runs with the pin already dropped.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn sealed_stat_mutation(module: &Path) {
    let regressed = fast_io::pinned_root::symlink_metadata(module)
        .expect_err("without the pin the lstat must re-walk the sealed parent and fail");
    assert!(
        eacces(&regressed),
        "expected EACCES without the pin, got {regressed}"
    );
}

/// No `O_PATH` here, so there is no anchored stat to assert on.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn sealed_stat_assertions(_module: &Path, _sub_file: &Path) {}

/// No `O_PATH` here, so there is no anchored stat to mutate away.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn sealed_stat_mutation(_module: &Path) {}

/// A module root the operator reached through their OWN symlink still pins.
///
/// The pin goes through the ownership walk, because upstream's `.` is the
/// directory `change_dir()` entered and a non-chrooted daemon enters it with
/// `open_no_attacker_symlinks()` (`util1.c:1254-1263`). That walk refuses a
/// component symlink owned by a foreign uid and FOLLOWS one owned by uid 0 or
/// our euid - the `/backup -> /mnt/disk` administrative pattern. Hardening the
/// pin into a blanket `O_NOFOLLOW` would refuse that pattern too, and this is
/// the half that would go quiet if it did.
///
/// The refusing half needs a symlink owned by a uid we are not, so it needs
/// root; upstream's own `testsuite/daemon-module-chdir-symlink_test.py` is that
/// cell, and `symlink_owner_is_trusted` pins the predicate.
///
/// A phase of the one test rather than a `#[test]` of its own: the session and
/// its pin are process globals, so two cells installing sessions concurrently
/// would answer each other's questions.
fn an_operator_owned_symlink_at_the_module_root_is_followed_and_pinned() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    fs::create_dir(&real).expect("mkdir real");
    fs::write(real.join("payload"), b"served\n").expect("write payload");
    let linked = temp.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("symlink the module root");

    install_daemon_session(ModuleState {
        root: Some(linked.clone()),
        chrooted: false,
        selected: true,
        insecure_links: ModuleInsecureLinks::default(),
    });
    pin_session_root_fd(&linked).expect("an operator-owned symlinked module root must still pin");

    let names = child_names(fast_io::pinned_root::read_dir(&linked).expect("scan through the pin"))
        .expect("entries");
    clear_session_root_fd();
    assert_eq!(
        names,
        vec![OsString::from("payload")],
        "the pin must resolve the operator's own symlink, not refuse it"
    );
}

/// `geteuid` has no safe wrapper in std; it takes no arguments, reads no
/// memory, and cannot fail.
fn unsafe_geteuid() -> u32 {
    // SAFETY: `geteuid(2)` is a pure read of the calling process's credentials.
    // It takes no pointers, has no failure mode, and is async-signal-safe.
    #[allow(unsafe_code)]
    unsafe {
        libc::geteuid()
    }
}
