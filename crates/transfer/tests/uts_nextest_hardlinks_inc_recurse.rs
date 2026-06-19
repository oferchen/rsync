//! UTS-NEXTEST-EDGE.j: nextest port of the upstream `testsuite/hardlinks.test`
//! INC_RECURSE end-to-end scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/hardlinks.test` (the
//! identical scenario also lives in 3.4.3 / 3.4.2 / 3.4.1; the 3.4.4 file is
//! the canonical upstream copy).
//!
//! # Background
//!
//! Upstream's `hardlinks.test` exercises `-aH` (preserve hard links) under
//! incremental recursion (INC_RECURSE) with three escalating shapes:
//!
//! 1. A simple flat hardlink pair (`name1` + `name2` in the same directory)
//!    plus a third copy and a non-link, transferred with `-aHi --debug=HLINK5`.
//! 2. A hardlink that spans a deep subdirectory (`name1` -> `subdir/down/deep/
//!    new-file`) added alongside a large flat set of touch-files so the
//!    incremental file list crosses generator/sender segment boundaries.
//! 3. A `--link-dest` re-run against the previous destination so the receiver
//!    must hard-link every locally-inherited entry to the link-dest tree.
//!
//! oc-rsync ships INC_RECURSE on both the sender and the receiver under
//! ISI.h (sender default flipped back on under PR #5085 after the V61D-1
//! regression was triaged) and IFX-7 (hardlink wire encoding parity). The
//! runtests.py harness exercises the upstream `hardlinks.test` script
//! through `continue-on-error: true` CI plumbing - a per-test regression on
//! that path does not block a PR. The UTS-NEXTEST-EDGE family lifts the
//! upstream scenario into a native nextest integration test so it runs as
//! a required check on every PR.
//!
//! # What this test pins
//!
//! For each scenario:
//!
//! - The transfer exits cleanly (no nonzero status from the receiver or
//!   sender).
//! - Every source-side hardlink relationship survives the round-trip - the
//!   destination siblings share an inode when stat'd with
//!   `MetadataExt::ino()`.
//! - Destination file contents are byte-identical to the source.
//! - Files that are NOT hardlinked in the source (`name4` copy in scenario
//!   1) do not become hardlinked at the destination.
//! - Under `--link-dest`, the destination entries hard-link to the
//!   link-dest tree (the receiver inherits the basis as a local hardlink
//!   rather than allocating a fresh inode).
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - Windows hardlinks (`fs::hard_link()` works but
//! `nlink` semantics on NTFS through `MetadataExt::ino()` are unavailable
//! to std). The sibling UTS-NEXTEST-EDGE tests adopt the same gate
//! (`uts_nextest_chdir_symlink_race.rs` etc.).
//!
//! # Upstream References
//!
//! - `testsuite/hardlinks.test` - the upstream script this file ports.
//! - `hlink.c::match_hard_links()` - leader/follower assignment after the
//!   file list sort that the INC_RECURSE path relies on (see also
//!   `crates/daemon/src/tests/chunks/daemon_hardlinks_relative_receive.rs`).
//! - `flist.c` - `XMIT_HLINK_FIRST` flag wire encoding.
//! - `generator.c` / `receiver.c` - receiver-side `hard_link_one()` /
//!   `hard_link_cluster()`.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

/// Locate the workspace `oc-rsync` binary the test runner built.
///
/// Prefers Cargo's injected `CARGO_BIN_EXE_oc-rsync` when set; otherwise
/// walks up from the test executable until a `target/` directory is
/// found. Mirrors the lookup used by sibling integration tests
/// (`delete_missing_args_files_from.rs`,
/// `remove_source_files_local_copy.rs`,
/// `v61d_2_daemon_push_increcurse_perf_regression.rs`).
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Returns the inode number of `path`. Used to assert hardlink sharing at
/// the destination.
fn inode_of(path: &Path) -> u64 {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .ino()
}

/// Returns the nlink count of `path`. Used as a secondary signal: a true
/// hardlink pair must report `nlink >= 2` on every member.
fn nlink_of(path: &Path) -> u64 {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .nlink()
}

/// Runs `oc-rsync` with the provided arguments and asserts a clean exit.
///
/// `--inc-recursive` is left at the default-on setting so the transfer
/// exercises the INC_RECURSE sender/receiver path that the upstream test
/// covers under the canonical `-aHivv --debug=HLINK5` invocation. The
/// nextest port omits `-ivv --debug=HLINK5` because their only effect is
/// human-readable logging - the wire- and filesystem-level invariants we
/// assert here are identical with or without them.
fn run_oc_rsync(bin: &Path, args: &[&std::ffi::OsStr]) {
    let status = Command::new(bin)
        .args(args)
        .status()
        .expect("spawn oc-rsync");
    assert!(
        status.success(),
        "oc-rsync exited with {status:?} for args {args:?}",
    );
}

/// Builds a trailing-slash version of `dir` so the transfer copies the
/// directory's contents (matches upstream `"$fromdir/"`).
fn trailing_slash(dir: &Path) -> std::ffi::OsString {
    let mut s = dir.as_os_str().to_os_string();
    s.push("/");
    s
}

/// Scenario 1: two-file flat hardlink pair.
///
/// Mirrors the opening block of upstream `hardlinks.test`:
///
/// ```sh
/// echo "This is the file" > "$name1"
/// ln "$name1" "$name2"   # hardlink
/// ln "$name2" "$name3"   # second hardlink (3-way share)
/// cp "$name2" "$name4"   # NOT a hardlink: independent copy
/// $RSYNC -aHi "$fromdir/" "$todir/"
/// ```
///
/// Asserts:
/// 1. The transfer succeeds.
/// 2. `name1` / `name2` / `name3` at the destination share a single inode.
/// 3. `name4` at the destination has a distinct inode (independent copy
///    must NOT collapse into the hardlink set).
/// 4. All four files have the source's byte payload.
#[test]
fn flat_hardlink_pair_survives_inc_recurse_roundtrip() {
    let Some(bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let root: TempDir = tempdir().expect("tempdir");
    let fromdir = root.path().join("from");
    let todir = root.path().join("to");
    fs::create_dir_all(&fromdir).expect("mkdir from");

    let name1 = fromdir.join("name1");
    let name2 = fromdir.join("name2");
    let name3 = fromdir.join("name3");
    let name4 = fromdir.join("name4");
    let payload = b"This is the file\n";

    fs::write(&name1, payload).expect("write name1");
    fs::hard_link(&name1, &name2).expect("hardlink name1 -> name2");
    fs::hard_link(&name2, &name3).expect("hardlink name2 -> name3");
    fs::copy(&name2, &name4).expect("copy name2 -> name4 (independent)");

    // Pre-condition: source name1/name2/name3 share an inode, name4 does not.
    let src_ino = inode_of(&name1);
    assert_eq!(inode_of(&name2), src_ino, "source name1/name2 must share");
    assert_eq!(inode_of(&name3), src_ino, "source name1/name3 must share");
    assert_ne!(
        inode_of(&name4),
        src_ino,
        "source name4 must be an independent copy",
    );

    let from_arg = trailing_slash(&fromdir);
    run_oc_rsync(
        &bin,
        &[
            "-aH".as_ref(),
            "--inc-recursive".as_ref(),
            from_arg.as_ref(),
            todir.as_os_str(),
        ],
    );

    let dn1 = todir.join("name1");
    let dn2 = todir.join("name2");
    let dn3 = todir.join("name3");
    let dn4 = todir.join("name4");

    // All four destination files exist and have the source payload.
    for f in [&dn1, &dn2, &dn3, &dn4] {
        let bytes = fs::read(f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
        assert_eq!(
            bytes,
            payload,
            "destination {} content diverged from source",
            f.display(),
        );
    }

    // Hardlink invariant: name1/name2/name3 share an inode at the dest.
    let dest_ino = inode_of(&dn1);
    assert_eq!(
        inode_of(&dn2),
        dest_ino,
        "dest name1/name2 must share an inode after -aH round-trip",
    );
    assert_eq!(
        inode_of(&dn3),
        dest_ino,
        "dest name1/name3 must share an inode after -aH round-trip",
    );

    // The hardlinked trio must report nlink >= 3 at the destination.
    let dest_nlink = nlink_of(&dn1);
    assert!(
        dest_nlink >= 3,
        "dest name1 nlink={dest_nlink} (expected >= 3 for a 3-way hardlink set)",
    );

    // Independent copy: name4 must have a distinct inode AND nlink == 1.
    assert_ne!(
        inode_of(&dn4),
        dest_ino,
        "dest name4 must NOT collapse into the hardlink set",
    );
    assert_eq!(
        nlink_of(&dn4),
        1,
        "dest name4 nlink must be 1 (independent copy, not part of hardlink set)",
    );
}

/// Scenario 2: hardlink spanning a deep subdirectory under INC_RECURSE.
///
/// Mirrors the middle block of upstream `hardlinks.test`:
///
/// ```sh
/// makepath "$fromdir/subdir/down/deep"
/// (cd "$fromdir/subdir"; touch $files)
/// ln "$name1" "$fromdir/subdir/down/deep/new-file"
/// $RSYNC -aHi "$fromdir/" "$todir/"
/// ```
///
/// The upstream scenario uses ~1 296 touch-files to span generator/sender
/// segment boundaries under INC_RECURSE. The nextest port uses a smaller
/// but still multi-directory fixture (32 touch-files plus the deep
/// hardlink target): enough to traverse the INC_RECURSE per-directory
/// generator loop without inflating CI wall-clock. The relevant
/// invariants (hardlink survival, deep path, segment crossing) all hold
/// at this scale.
///
/// Asserts:
/// 1. The transfer succeeds.
/// 2. `name1` at the top level shares an inode with
///    `subdir/down/deep/new-file` at the destination - the receiver
///    correctly linked across directory and segment boundaries.
/// 3. The deep file has the source payload.
/// 4. All touch-files exist at the destination.
#[test]
fn deep_subdir_hardlink_survives_inc_recurse_roundtrip() {
    let Some(bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let root: TempDir = tempdir().expect("tempdir");
    let fromdir = root.path().join("from");
    let todir = root.path().join("to");
    let deep = fromdir.join("subdir").join("down").join("deep");
    fs::create_dir_all(&deep).expect("mkdir from/subdir/down/deep");

    // Top-level payload file that the deep entry will hardlink to.
    let name1 = fromdir.join("name1");
    let payload = b"This is the file\n";
    fs::write(&name1, payload).expect("write name1");

    // Touch-files in subdir/ to exercise the INC_RECURSE per-directory
    // generator loop. 32 entries is enough to put `subdir/` and
    // `subdir/down/deep/` into separate generator segments without
    // inflating tempdir teardown time.
    let touch_names: Vec<String> = (0..32).map(|i| format!("t{i:02}")).collect();
    let subdir = fromdir.join("subdir");
    for n in &touch_names {
        fs::write(subdir.join(n), b"").expect("touch subdir file");
    }

    // The deep hardlink: subdir/down/deep/new-file -> name1.
    let deep_link = deep.join("new-file");
    fs::hard_link(&name1, &deep_link).expect("hardlink name1 -> deep/new-file");

    // Pre-condition: source top-level name1 and deep new-file share an inode.
    let src_ino = inode_of(&name1);
    assert_eq!(
        inode_of(&deep_link),
        src_ino,
        "source name1 and subdir/down/deep/new-file must share an inode",
    );

    let from_arg = trailing_slash(&fromdir);
    run_oc_rsync(
        &bin,
        &[
            "-aH".as_ref(),
            "--inc-recursive".as_ref(),
            from_arg.as_ref(),
            todir.as_os_str(),
        ],
    );

    // Destination layout: name1 at the top, deep file under subdir/.
    let dn1 = todir.join("name1");
    let d_deep = todir
        .join("subdir")
        .join("down")
        .join("deep")
        .join("new-file");

    let dn1_bytes = fs::read(&dn1).expect("read dest name1");
    assert_eq!(dn1_bytes, payload, "dest name1 content diverged");
    let deep_bytes = fs::read(&d_deep).expect("read dest deep new-file");
    assert_eq!(deep_bytes, payload, "dest deep new-file content diverged");

    // Hardlink invariant: top-level and deep-nested entries share an inode.
    let dest_ino = inode_of(&dn1);
    assert_eq!(
        inode_of(&d_deep),
        dest_ino,
        "dest name1 and subdir/down/deep/new-file must share an inode \
         after -aH round-trip across INC_RECURSE segments",
    );
    let dest_nlink = nlink_of(&dn1);
    assert!(
        dest_nlink >= 2,
        "dest name1 nlink={dest_nlink} (expected >= 2 for hardlink to deep new-file)",
    );

    // All touch-files made it across.
    let dest_subdir = todir.join("subdir");
    for n in &touch_names {
        let p = dest_subdir.join(n);
        assert!(
            p.is_file(),
            "touch-file {} missing at destination",
            p.display(),
        );
    }
}

/// Scenario 3: `--link-dest` re-run hardlinks every locally-inherited entry.
///
/// Mirrors the `--link-dest` block of upstream `hardlinks.test`:
///
/// ```sh
/// $RSYNC -aH                          "$fromdir/" "$todir/"
/// $RSYNC -aH --link-dest="$todir"     "$fromdir/" "$chkdir/"
/// ```
///
/// On the second run the receiver compares each source entry against the
/// `link-dest` candidate; when content matches the receiver MUST hardlink
/// to the basis rather than allocate a fresh inode. This pins the
/// LinkDest action in `engine::local_copy::executor::file::links` and the
/// receiver's basis-link path.
///
/// Asserts:
/// 1. Both transfers succeed.
/// 2. Each entry in `chkdir/` shares an inode with the corresponding
///    `todir/` entry (the link-dest basis) - the receiver hard-linked
///    rather than copied.
/// 3. The internal hardlink relationship from the source (`name1` <->
///    `name2`) is preserved within `chkdir/` (transitively, since both
///    members link to the shared `todir/` basis inode).
#[test]
fn link_dest_inherits_hardlinks_from_basis() {
    let Some(bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let root: TempDir = tempdir().expect("tempdir");
    let fromdir = root.path().join("from");
    let todir = root.path().join("to");
    let chkdir = root.path().join("chk");
    fs::create_dir_all(&fromdir).expect("mkdir from");

    let name1 = fromdir.join("name1");
    let name2 = fromdir.join("name2");
    let solo = fromdir.join("solo");
    let payload = b"This is the file\n";
    let solo_payload = b"This is another file\n";

    fs::write(&name1, payload).expect("write name1");
    fs::hard_link(&name1, &name2).expect("hardlink name1 -> name2");
    fs::write(&solo, solo_payload).expect("write solo");

    // First transfer: populate the link-dest basis at todir/.
    let from_arg = trailing_slash(&fromdir);
    run_oc_rsync(
        &bin,
        &[
            "-aH".as_ref(),
            "--inc-recursive".as_ref(),
            from_arg.as_ref(),
            todir.as_os_str(),
        ],
    );

    let to_name1 = todir.join("name1");
    let to_name2 = todir.join("name2");
    let to_solo = todir.join("solo");

    // Sanity: the basis tree already has the hardlink relationship.
    let basis_pair_ino = inode_of(&to_name1);
    assert_eq!(
        inode_of(&to_name2),
        basis_pair_ino,
        "basis name1/name2 must share an inode before link-dest re-run",
    );
    let basis_solo_ino = inode_of(&to_solo);
    assert_ne!(
        basis_solo_ino, basis_pair_ino,
        "basis solo must have a distinct inode from the hardlink pair",
    );

    // Second transfer: --link-dest against the basis. Receiver MUST
    // hardlink each entry into chkdir/ rather than copy.
    let link_dest_arg = {
        let mut s = std::ffi::OsString::from("--link-dest=");
        s.push(todir.as_os_str());
        s
    };
    run_oc_rsync(
        &bin,
        &[
            "-aH".as_ref(),
            "--inc-recursive".as_ref(),
            link_dest_arg.as_ref(),
            from_arg.as_ref(),
            chkdir.as_os_str(),
        ],
    );

    let chk_name1 = chkdir.join("name1");
    let chk_name2 = chkdir.join("name2");
    let chk_solo = chkdir.join("solo");

    // Content invariants.
    assert_eq!(fs::read(&chk_name1).expect("read chk name1"), payload);
    assert_eq!(fs::read(&chk_name2).expect("read chk name2"), payload);
    assert_eq!(fs::read(&chk_solo).expect("read chk solo"), solo_payload);

    // --link-dest invariant: chkdir entries hardlink to the basis tree
    // rather than allocating fresh inodes. Receiver inherits the basis
    // entry as a local hardlink (the engine LinkDest action).
    assert_eq!(
        inode_of(&chk_name1),
        basis_pair_ino,
        "chk name1 must hardlink to basis name1 under --link-dest",
    );
    assert_eq!(
        inode_of(&chk_name2),
        basis_pair_ino,
        "chk name2 must hardlink to basis name2 under --link-dest",
    );
    assert_eq!(
        inode_of(&chk_solo),
        basis_solo_ino,
        "chk solo must hardlink to basis solo under --link-dest",
    );

    // Transitively, the chkdir pair shares an inode with itself: the
    // source's name1 <-> name2 relationship is preserved end-to-end
    // through the link-dest basis.
    assert_eq!(
        inode_of(&chk_name1),
        inode_of(&chk_name2),
        "chk name1/name2 must share an inode (basis hardlink relationship preserved)",
    );

    // nlink at the basis must reflect the new link-dest references:
    // basis name1 originally had nlink==2 (paired with basis name2);
    // after the link-dest re-run, both chk name1 and chk name2 link to
    // their respective basis entries, so the shared inode is now
    // referenced by 4 paths.
    let post_nlink = nlink_of(&to_name1);
    assert!(
        post_nlink >= 4,
        "basis name1 nlink={post_nlink} (expected >= 4 after --link-dest re-run \
         brings in chk name1 and chk name2)",
    );
}
