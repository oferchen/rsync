//! Module-root confinement of an operator/peer path that resolves out of the
//! tree through a *trusted* symlink.
//!
//! # Why this matters
//!
//! The ownership walk deliberately FOLLOWS a symlink owned by uid 0 or our own
//! euid: that is the operator's own layout (`/var/log -> /data/log`) and
//! refusing it would break the ordinary case the resolver exists to keep
//! working. Ownership therefore cannot be the whole defence for a *peer-driven*
//! path. Upstream states the residual exactly, at the merge-file open:
//!
//! > Confine the open to the module root. The ownership walk on its own is not
//! > enough for a peer-driven merge file: a non-chrooted daemon writes
//! > `--backup-dir` entries as root, so a raced backup symlink is ROOT-owned -
//! > exactly what `open_no_attacker_symlinks()` treats as trusted - and naming
//! > it in a dir-merge rule would read an out-of-module file in as filter rules
//! > (their text comes back to the peer in "Unknown filter rule" errors).
//!
//! So the refusal must come from the CONFINEMENT ROOT, after the follow, not
//! from ownership. That is the property pinned here.
//!
//! # Root ownership is not a separate branch
//!
//! Upstream refuses only `st_uid != 0 && st_uid != trusted_uid`
//! (`syscall.c:406`), so uid 0 and the euid take one identical follow path and
//! reach one identical confinement check. A plant owned by our own euid
//! exercises the same code as a root-owned one and needs no privilege, which is
//! why these run unprivileged. The distinct third-uid REFUSAL - the arm that
//! does need a second uid - stays with the root leg of the upstream 3.5.0
//! testsuite, matching the convention already written down in
//! `tests/owner_walk.rs`.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:286` `ona_open()` - the walk; `abspath` is seeded
//!   from `module_dir` for a daemon at `:308-310`.
//! - `rsync-3.5.0/syscall.c:406` - the ownership test that follows a trusted
//!   symlink.
//! - `rsync-3.5.0/syscall.c:460-468` - the leaf arm: `abspath_step()` then
//!   `abspath_outside_confinement()` -> `ELOOP`.
//! - `rsync-3.5.0/syscall.c:186-240` `abspath_outside_confinement()` - refuses
//!   only when `operator_path_resolve` is set.
//! - `rsync-3.5.0/exclude.c:1668-1684` `parse_filter_file()` - the merge-file
//!   open that sets `operator_path_resolve = 1`, exempting only the daemon's
//!   own `filter`/`include from`/`exclude from` parameters.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use fast_io::confinement::{ModuleInsecureLinks, ModuleState};
use tempfile::TempDir;

/// The session's confinement root is process-global, mirroring upstream's
/// `module_dir`. Each cell installs the session it needs, so they must not
/// interleave when the harness runs them as threads.
static SESSION: Mutex<()> = Mutex::new(());

/// A module root, a directory beneath it, and a secret outside it.
struct Fixture {
    _root: TempDir,
    module: PathBuf,
    secret: PathBuf,
}

fn fixture() -> Fixture {
    let root = TempDir::new().expect("tempdir");
    let module = root.path().join("module");
    fs::create_dir_all(module.join("backup/sub")).expect("mkdir module");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).expect("mkdir outside");
    let secret = outside.join("secret");
    fs::write(&secret, "SECRET-MARKER").expect("write secret");
    Fixture {
        _root: root,
        module,
        secret,
    }
}

/// Serve `module` as a non-chrooted daemon module with the confinement engaged.
fn serve_module(module: &Path) -> MutexGuard<'static, ()> {
    let guard = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    fast_io::confinement::install_daemon_session(ModuleState {
        root: Some(module.to_path_buf()),
        chrooted: false,
        selected: true,
        insecure_links: ModuleInsecureLinks::from_module_config(false),
    });
    guard
}

fn read_all(mut file: fs::File) -> String {
    let mut contents = String::new();
    file.read_to_string(&mut contents).expect("read");
    contents
}

/// THE PIN. A trusted-owned symlink planted inside the module points at a file
/// outside it. The walk follows the link - that is correct and deliberate - and
/// then the module root must refuse the resolved target, so the secret never
/// becomes filter-rule text the peer can read back.
#[test]
fn a_trusted_symlink_leaving_the_module_is_refused() {
    let fixture = fixture();
    let planted = fixture.module.join("backup/sub/evil");
    symlink(&fixture.secret, &planted).expect("plant the symlink");

    let _session = serve_module(&fixture.module);
    let error = fast_io::operator_open_read_confined(&planted)
        .expect_err("a confined open must not resolve out of the module");

    assert_eq!(
        error.raw_os_error(),
        Some(libc::ELOOP),
        "the refusal must be ELOOP: callers treat EXDEV as cross-device and \
         fall back to copy+remove, which would launder it"
    );
}

/// The plant is real at the moment of the run, and it is trusted. Without this
/// the pin above would also pass if the symlink had simply failed to appear, or
/// if it were refused on OWNERSHIP - the arm that is not under test here.
#[test]
fn the_plant_is_present_and_trusted_owned() {
    let fixture = fixture();
    let planted = fixture.module.join("backup/sub/evil");
    symlink(&fixture.secret, &planted).expect("plant the symlink");

    let meta = fs::symlink_metadata(&planted).expect("the plant must exist");
    assert!(meta.file_type().is_symlink(), "the plant must be a symlink");
    assert!(
        fast_io::symlink_owner_is_trusted(std::os::unix::fs::MetadataExt::uid(&meta)),
        "the plant must be TRUSTED-owned, or the refusal proves nothing about \
         confinement - it would only be the ownership arm firing"
    );

    // The target really does hold the secret, so a leak would be observable.
    assert_eq!(
        read_all(fs::File::open(&planted).expect("plain open follows the link")),
        "SECRET-MARKER"
    );
}

/// NEGATIVE CONTROL for over-refusal. A trusted symlink whose target stays
/// INSIDE the module is still followed. Without this the pin would be satisfied
/// by refusing every symlink, which is a different - and wrong - resolver.
#[test]
fn a_trusted_symlink_staying_inside_the_module_is_followed() {
    let fixture = fixture();
    let inside = fixture.module.join("data");
    fs::write(&inside, "IN-MODULE").expect("write in-module target");
    let link = fixture.module.join("backup/sub/benign");
    symlink(&inside, &link).expect("plant the in-module symlink");

    let _session = serve_module(&fixture.module);
    let file = fast_io::operator_open_read_confined(&link)
        .expect("an in-module target must still be followed");

    assert_eq!(read_all(file), "IN-MODULE");
}

/// NON-VACUITY companion. With no symlink in play a confined open is an
/// ordinary resolution. Without this, every refusal above would also hold if
/// the confined walk simply failed for all inputs.
#[test]
fn a_plain_path_inside_the_module_opens() {
    let fixture = fixture();
    let plain = fixture.module.join("backup/sub/plain");
    fs::write(&plain, "PLAIN").expect("write");

    let _session = serve_module(&fixture.module);
    let file = fast_io::operator_open_read_confined(&plain).expect("plain resolution");

    assert_eq!(read_all(file), "PLAIN");
}

/// The other half of upstream's rule: the confinement applies to CONFINED opens
/// only. `--log-file`, the `--*-from` family and the daemon's lock and motd
/// files may legitimately live outside the tree, so the ANCILLARY entry point
/// must still reach the same target the confined one refused.
///
/// Pinned because a blanket confinement would satisfy every assertion above
/// while creating a new divergence in the opposite direction.
///
/// upstream: `rsync-3.5.0/syscall.c:232-239` - "other opens (--log-file,
/// --*-from, lock/motd) may legitimately live elsewhere".
#[test]
fn an_ancillary_open_may_still_leave_the_module() {
    let fixture = fixture();
    let planted = fixture.module.join("backup/sub/evil");
    symlink(&fixture.secret, &planted).expect("plant the symlink");

    let _session = serve_module(&fixture.module);
    let file = fast_io::operator_open_read(&planted)
        .expect("an ancillary path is not bound to the module root");

    assert_eq!(read_all(file), "SECRET-MARKER");
}

/// With no confinement root there is nothing to be outside of, so a confined
/// open behaves exactly like an unconfined one. This is the plain local client:
/// upstream's `confinement_root()` returns `confine_root`, which is unset.
///
/// upstream: `rsync-3.5.0/syscall.c:128-144` `confinement_root()`.
#[test]
fn without_a_confinement_root_a_confined_open_still_follows() {
    let fixture = fixture();
    let planted = fixture.module.join("backup/sub/evil");
    symlink(&fixture.secret, &planted).expect("plant the symlink");

    let _guard = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    fast_io::confinement::install_local_session(
        fast_io::confinement::LocalInsecureLinks::from_local_flag(false),
    );
    let file = fast_io::operator_open_read_confined(&planted)
        .expect("no root means nothing is outside it");

    assert_eq!(read_all(file), "SECRET-MARKER");
}
