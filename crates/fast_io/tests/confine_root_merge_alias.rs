//! `--confine-root` bounds a merge-file open KERNEL-ANCHORED, not lexically.
//!
//! # The shape
//!
//! A dir-merge rule travels over the protocol as a filter rule, not in the argv
//! an `rrsync` wrapper validated, so the peer names the merge file. Bounding
//! that open is the job of the confinement root - and the interesting case is
//! the one where a STRING comparison gets the wrong answer.
//!
//! Reach the tree through a symlink that points at its own directory
//! (`root/self-alias -> .`) and rsync's tracked path sits one component deeper
//! than the kernel's. A merge file's own relative `..` then pops a component
//! that the kernel never descended:
//!
//! ```text
//!   named:    <root>/self-alias/outside-link
//!   link:     outside-link -> ../secret
//!   LEXICAL:  <root>/self-alias/../secret  ==  <root>/secret     -> inside  ✗
//!   KERNEL:   self-alias resolves to <root>, then ../secret      -> outside ✓
//! ```
//!
//! So a resolver that normalises the string lands INSIDE the root and admits
//! the read; only one that advances its tracked path by RESOLVED components
//! sees the escape. [`lexical_normalisation_lands_inside_the_root`] pins that
//! the fixture really does have this property, so the refusal below is
//! evidence about the mechanism and not about an easier path.
//!
//! # Why the root and not ownership
//!
//! Every link here is owned by the euid running the test, which the walk
//! FOLLOWS by design (`syscall.c:406`) - refusing an operator's own layout
//! would break the ordinary case. The refusal therefore has to come from the
//! confinement root, after the follow.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:316-328` - the `confine_root` arm seeds `abspath`
//!   from `getcwd()` (`:327`), "It must be the PHYSICAL cwd: `curr_dir` is the lexical
//!   name `change_dir()` was given, so after descending a trusted symlink the
//!   tracker sits at a different depth than the kernel, and a `..` that really
//!   escapes looks like it landed inside."
//! - `rsync-3.5.0/syscall.c:245` `abspath_step()` - advances one RESOLVED
//!   component at a time.
//! - `rsync-3.5.0/syscall.c:186-240` `abspath_outside_confinement()`.
//! - `rsync-3.5.0/exclude.c:1680-1684` `parse_filter_file()` - the merge-file
//!   open this bounds.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};

use tempfile::TempDir;

/// The session's confinement root is process-global, mirroring upstream's
/// `confine_root`. Each cell installs the session it needs, so they must not
/// interleave when the harness runs them as threads.
static SESSION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A confine root reached through a self-alias, a secret beside it, and both an
/// escaping and an in-tree merge file inside.
struct Aliased {
    _base: TempDir,
    root: PathBuf,
    secret: PathBuf,
}

fn aliased() -> Aliased {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("aliased");
    fs::create_dir(&root).expect("mkdir root");

    let secret = base.path().join("aliased-secret");
    fs::write(&secret, "ALIASED-SECRET").expect("write the outside secret");

    // The alias that makes the tracked path deeper than the kernel's.
    symlink(".", root.join("self-alias")).expect("plant the self alias");
    // The escape: a relative link whose `..` is spent against the REAL parent.
    symlink("../aliased-secret", root.join("outside-link")).expect("plant the escape");
    // The control: an ordinary merge file inside the root.
    fs::write(root.join("intree-list"), "IN-TREE-RULES").expect("write the in-tree merge file");

    Aliased {
        _base: base,
        root,
        secret,
    }
}

/// Confine to `root` the way a `--confine-root` server does.
fn confine_to(root: &Path) -> std::sync::MutexGuard<'static, ()> {
    let guard = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    fast_io::confinement::install_local_session(
        fast_io::confinement::LocalInsecureLinks::from_local_flag(false),
        Some(root.to_path_buf()),
    );
    guard
}

/// Collapse `.` and `..` textually - what a resolver that never touches the
/// filesystem would compute.
fn lexically_normalised(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// THE INSTRUMENT CHECK, and it is what makes the pin below mean anything: the
/// escaping path normalises to a location INSIDE the root, so a lexical
/// resolver would admit the read. Without this cell the refusal could just as
/// well be a string comparison getting an easy case right.
#[test]
fn lexical_normalisation_lands_inside_the_root() {
    let fixture = aliased();
    let named = fixture.root.join("self-alias").join("outside-link");

    // The named path itself, before the leaf link is expanded.
    assert!(
        lexically_normalised(&named).starts_with(&fixture.root),
        "the fixture must name a path that looks in-tree textually"
    );
    // And with the leaf link's own target spliced in the way a string-based
    // resolver would splice it: `<root>/self-alias/../aliased-secret`.
    let spliced = named.parent().expect("parent").join("../aliased-secret");
    assert!(
        lexically_normalised(&spliced).starts_with(&fixture.root),
        "the fixture's escape must be invisible to a lexical check, or the \
         refusal below proves nothing about kernel anchoring"
    );

    // ...while the kernel really does land outside.
    assert_eq!(
        fs::canonicalize(&named).expect("the escape resolves"),
        fs::canonicalize(&fixture.secret).expect("the secret resolves"),
        "the named path must really reach the outside secret"
    );
}

/// THE PIN. The merge-file read is refused because the walk tracks where the
/// open would REALLY land, having followed `self-alias` to the root itself.
#[test]
fn a_merge_file_escaping_through_a_self_alias_is_refused() {
    let fixture = aliased();
    let named = fixture.root.join("self-alias").join("outside-link");

    let _session = confine_to(&fixture.root);
    let error = fast_io::operator_read_to_string_confined(&named)
        .expect_err("a merge file resolving outside --confine-root must be refused");

    assert_eq!(
        error.raw_os_error(),
        Some(libc::ELOOP),
        "the refusal must be ELOOP so callers tell it from an ordinary \
         open failure; got {error:?}"
    );
}

/// NON-VACUITY companion, and the one that matters: reaching the SAME
/// directory through the SAME alias, an in-tree merge file must still be read.
/// An over-refusing resolver would satisfy the pin above while breaking every
/// ordinary transfer whose source argument goes through a symlink.
#[test]
fn an_in_tree_merge_file_through_the_same_alias_is_still_read() {
    let fixture = aliased();
    let named = fixture.root.join("self-alias").join("intree-list");

    let _session = confine_to(&fixture.root);
    let contents = fast_io::operator_read_to_string_confined(&named)
        .expect("an in-tree merge file must still be read through the alias");

    assert_eq!(contents, "IN-TREE-RULES");
}

/// The plants are real and TRUSTED-owned, so the refusal above is the
/// confinement arm and not the ownership arm firing.
#[test]
fn the_alias_and_the_escape_are_trusted_owned_symlinks() {
    use std::os::unix::fs::MetadataExt as _;

    let fixture = aliased();
    for name in ["self-alias", "outside-link"] {
        let meta = fs::symlink_metadata(fixture.root.join(name)).expect("the plant must exist");
        assert!(
            meta.file_type().is_symlink(),
            "{name} must be a symlink, or the fixture does not have the shape \
             the pin describes"
        );
        assert!(
            fast_io::symlink_owner_is_trusted(meta.uid()),
            "{name} must be TRUSTED-owned: an untrusted link would be refused \
             on ownership and the confinement arm would never run"
        );
    }
}

/// The escape is refused only while a root is installed. With none - the plain
/// local client, upstream's NULL `confine_root` - the same read succeeds, which
/// is what shows the refusal comes from the root and not from the alias.
#[test]
fn without_a_root_the_same_read_succeeds() {
    let fixture = aliased();
    let named = fixture.root.join("self-alias").join("outside-link");

    let _guard = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    fast_io::confinement::install_local_session(
        fast_io::confinement::LocalInsecureLinks::from_local_flag(false),
        None,
    );
    let contents = fast_io::operator_read_to_string_confined(&named)
        .expect("no root means nothing is outside it");

    assert_eq!(contents, "ALIASED-SECRET");
}

/// The ANCILLARY entry point keeps its own policy: `--log-file`, the
/// `--*-from` family and the daemon's lock/motd files may legitimately live
/// outside the tree, so widening the merge-file rule to every operator open
/// would be a divergence in the opposite direction.
///
/// upstream: `rsync-3.5.0/syscall.c:232-239`.
#[test]
fn an_ancillary_read_is_not_bound_by_the_root() {
    let fixture = aliased();
    let named = fixture.root.join("self-alias").join("outside-link");

    let _session = confine_to(&fixture.root);
    let contents = fast_io::operator_read_to_string(&named)
        .expect("an ancillary path is not bound to the confinement root");

    assert_eq!(contents, "ALIASED-SECRET");
}
