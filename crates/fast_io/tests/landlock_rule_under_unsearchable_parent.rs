//! The Landlock allowlist must still INSTALL for a module under an
//! unsearchable ancestor.
//!
//! `restrict_to_module_paths` opens each rule path to hand the kernel a
//! descriptor, and it runs after the daemon's privilege drop. Opening the
//! module by its absolute path there re-traverses the module's ancestors under
//! the dropped identity, so a module beneath a directory that identity cannot
//! search (`path = /home/backup/data` under a 0700 home) fails with `EACCES`
//! before a single rule is added.
//!
//! The caller only WARNS on that failure and serves the connection anyway
//! (`crates/daemon/.../transfer/sandbox.rs`, the `LandlockOutcome::Error`
//! arm), which is the part that makes this worth its own cell: the module is
//! then served with no kernel sandbox at all, and the hardening disappears on
//! exactly the layout it was added for. "The transfer succeeded" cannot see
//! that - it succeeds either way - so the outcome value is asserted directly.
//!
//! The barrier is built with `chmod 0` rather than a uid drop: the kernel rule
//! is search permission on an ancestor, and a uid is only the usual way of
//! losing it. Root is exempt from both, so the cell skips there.
//!
//! upstream: `clientserver.c:1059-1065` - the pin, taken before the drop.

#![cfg(all(target_os = "linux", feature = "landlock"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;

use fast_io::confinement::{
    ModuleInsecureLinks, ModuleState, clear_session_root_fd, install_daemon_session,
    pin_session_root_fd,
};
use fast_io::landlock::{LandlockOutcome, is_supported, restrict_to_module_paths};

/// Ask the kernel to install the allowlist for `module`, on a thread of its
/// own.
///
/// Landlock restricts the calling THREAD irreversibly, so a scenario that
/// succeeds must not leave the test's own thread sandboxed - it still has a
/// fixture to tear down, and a restricted thread cannot unlink its own parent
/// directory.
fn install_on_a_worker(module: &Path, drop_pin_first: bool) -> Result<LandlockOutcome, String> {
    let module: PathBuf = module.to_path_buf();
    thread::Builder::new()
        .name("landlock-pinned-root".into())
        .spawn(move || {
            if drop_pin_first {
                clear_session_root_fd();
            }
            restrict_to_module_paths(&[module.as_path()])
        })
        .map_err(|e| format!("spawn worker: {e}"))?
        .join()
        .map_err(|_| "worker thread panicked".to_owned())
}

#[test]
fn the_ruleset_installs_for_a_module_whose_parent_is_unsearchable() {
    // SAFETY: `geteuid(2)` is a pure read of the calling process's
    // credentials. It takes no pointers and has no failure mode.
    #[allow(unsafe_code)]
    let euid = unsafe { libc::geteuid() };
    if euid == 0 || !is_supported() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let private = temp.path().join("private");
    let module = private.join("mod");
    fs::create_dir_all(module.join("sub")).expect("mkdir module");
    fs::write(module.join("sub").join("file"), b"served\n").expect("write payload");
    fs::set_permissions(&module, fs::Permissions::from_mode(0o755)).expect("chmod module");

    let seal = |mode: u32| {
        fs::set_permissions(&private, fs::Permissions::from_mode(mode)).expect("chmod parent");
    };
    let publish_and_pin = || {
        install_daemon_session(ModuleState {
            root: Some(module.clone()),
            chrooted: false,
            selected: true,
            insecure_links: ModuleInsecureLinks::default(),
        });
        pin_session_root_fd(&module).expect("pin the module root");
    };

    // Every outcome is collected first and asserted at the end, so a failure
    // never leaves the fixture sealed shut and untearable-down.

    // 1. Non-vacuity: an ordinary, traversable parent, no pin at all. A change
    //    that broke the everyday deployment would otherwise be invisible here.
    clear_session_root_fd();
    let ordinary = install_on_a_worker(&module, false);

    // 2. The mutation: the barrier up, the pin dropped. This is what the code
    //    did before the pin existed.
    publish_and_pin();
    seal(0o000);
    let unpinned = install_on_a_worker(&module, true);
    seal(0o755);

    // 3. The assertion: the same call, same barrier, with the pin taken while
    //    the parent was still searchable.
    publish_and_pin();
    seal(0o000);
    let pinned = install_on_a_worker(&module, false);
    seal(0o755);
    clear_session_root_fd();

    assert!(
        matches!(ordinary, Ok(LandlockOutcome::Enforced(_))),
        "an unpinned module under a traversable parent must still be sandboxed: {ordinary:?}"
    );
    match unpinned {
        Ok(LandlockOutcome::Error(err)) if err.raw_os_error() == Some(libc::EACCES) => {}
        other => panic!(
            "without the pin the rule path open must EACCES on the sealed parent, got {other:?}"
        ),
    }
    assert!(
        matches!(pinned, Ok(LandlockOutcome::Enforced(_))),
        "the ruleset must install for a module under an unsearchable parent: {pinned:?}"
    );

    drop(temp);
}
